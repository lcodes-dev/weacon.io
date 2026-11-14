use std::convert::Infallible;

use axum::Extension;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::timeout;
use weacon_observability::{Fault, Field};
use weacon_protocol::{Capability, Code, Message, WorkspaceChange, WorkspaceId, code};

use crate::auth::Member;
use crate::connections::Place;
use crate::http::{Shared, failure};
use crate::socket::{Closed, Pings, greet, reply};

/// What a client must offer, and the server offers, to get a workspace's changes.
const FEED: Code = code!("workspace.feed");
/// What the server offers to clients that read and change tasks.
const TASKS: Code = code!("workspace.tasks");
/// What the server offers to clients that read and revise docs.
const DOCS: Code = code!("workspace.docs");

const MAX_MESSAGE: usize = 16 * 1024;
const PAGE: u32 = 500;

#[derive(Deserialize)]
pub(crate) struct Since {
    #[serde(default)]
    since: u64,
}

/// `GET /api/workspaces/{workspace}/feed`: a member's WebSocket of the workspace's changes
/// after `since`, then each new one as it happens.
pub(crate) async fn feed(
    State(shared): State<Shared>,
    Query(Since { since }): Query<Since>,
    Extension(place): Extension<Place>,
    member: Member,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Subscribed before the backlog is read, so no change falls between the two.
    let changes = shared.changes.subscribe();
    // A member joins a workspace when they first open it.
    match member.workspace.join(&member.id).await {
        Ok(true) => shared.changed(member.workspace.id()),
        Ok(false) => {}
        Err(fault) => return failure(fault),
    }
    let feed = Feed {
        _place: place,
        member,
        changes,
        last_seq: since,
        offers: Offers::default(),
    };
    upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve(socket, shared, feed))
}

struct Feed {
    /// The feed's place among the open connections.
    _place: Place,
    member: Member,
    /// Which workspace changed; the changes themselves are read from the database, in order.
    changes: Receiver<WorkspaceId>,
    last_seq: u64,
    offers: Offers,
}

/// Which features the client offered besides the feed; it hears only of their changes.
#[derive(Default)]
struct Offers {
    tasks: bool,
    docs: bool,
}

impl Feed {
    /// Goes on only while the feed's session is live and its account still a member.
    async fn allowed(&self, shared: &Shared) -> Result<(), Closed> {
        let Member {
            workspace,
            token_hash,
            ..
        } = &self.member;
        match workspace.session_member(token_hash, shared.now()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(Closed),
            Err(fault) => Err(feed_failed(shared, workspace.id(), &fault)),
        }
    }
}

/// Warns that the feed of `workspace` failed with `fault`, which ends it.
fn feed_failed(shared: &Shared, workspace: &WorkspaceId, fault: &Fault) -> Closed {
    shared.log.warn(
        "feed failed",
        &[
            ("workspace_id", workspace.as_str().into()),
            ("err", Field::error(fault)),
        ],
    );
    Closed
}

async fn serve(mut socket: WebSocket, shared: Shared, mut feed: Feed) {
    // However the feed ends, it just ends: the client sees its socket close.
    let Err(Closed) = run(&mut socket, &shared, &mut feed).await;
}

async fn run(
    socket: &mut WebSocket,
    shared: &Shared,
    feed: &mut Feed,
) -> Result<Infallible, Closed> {
    let [feed_offer, tasks, docs] = [FEED, TASKS, DOCS].map(Capability::from);
    let offered = vec![feed_offer.clone(), tasks.clone(), docs.clone()];
    let greeted = greet(socket, shared, offered, &feed_offer);
    let hello = timeout(shared.sockets.hello_within, greeted)
        .await
        .map_err(|_| Closed)??;
    feed.offers = Offers {
        tasks: hello.capabilities.contains(&tasks),
        docs: hello.capabilities.contains(&docs),
    };
    catch_up(socket, shared, feed).await?;
    let mut pings = Pings::new(shared.sockets.ping_every);
    loop {
        tokio::select! {
            change = feed.changes.recv() => match change {
                Ok(id) if id != *feed.member.workspace.id() => {}
                // A feed that fell behind just reads more from the database.
                Ok(_) | Err(RecvError::Lagged(_)) => catch_up(socket, shared, feed).await?,
                Err(RecvError::Closed) => return Err(Closed),
            },
            () = pings.due() => pings.ping(socket, feed.allowed(shared)).await?,
            frame = socket.recv() => pings.heard(&frame.ok_or(Closed)?)?,
        }
    }
}

/// Sends every change after the last one sent, a page at a time, while its member still is one.
async fn catch_up(socket: &mut WebSocket, shared: &Shared, feed: &mut Feed) -> Result<(), Closed> {
    loop {
        let workspace = &feed.member.workspace;
        let page = workspace
            .changes_since(feed.last_seq, PAGE)
            .await
            .map_err(|fault| feed_failed(shared, workspace.id(), &fault))?;
        let more = usize::try_from(PAGE).is_ok_and(|full| page.len() == full);
        for update in page {
            feed.last_seq = update.seq;
            let wanted = match update.change {
                WorkspaceChange::TaskChanged { .. } => feed.offers.tasks,
                WorkspaceChange::DocChanged { .. } => feed.offers.docs,
                WorkspaceChange::MemberJoined { .. } | WorkspaceChange::Newer => true,
            };
            if !wanted {
                continue;
            }
            // Before each one, not each page: a member removed meanwhile gets nothing more.
            feed.allowed(shared).await?;
            reply(socket, shared, 0, Message::Workspace(update)).await?;
        }
        if !more {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use std::time::Duration;

    use futures_util::SinkExt;
    use tokio::io::AsyncReadExt;
    use tokio::sync::Semaphore;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::{self, Message as Wire};
    use tokio_tungstenite::{MaybeTlsStream, connect_async};
    use weacon_protocol::{MemberId, PROTOCOL, WorkspaceChange, WorkspaceUpdate};

    use weacon_model::{Attribution, Doc, DocText, OrderKey, Task, Title};
    use weacon_protocol::{DocId, TaskId};

    use super::*;
    use crate::auth::SESSION_COOKIE;
    use crate::db::Workspace;
    use crate::email::Email;
    use crate::sign_in::SESSION_LIFETIME;
    use crate::socket::error;
    use crate::test_support::socket::{Client, ends, frame, next, open, say, say_as};
    use crate::test_support::{
        self, Fixture, Running, fresh_account, member_id, run, workspace_id,
    };

    // For tests whose client stops reading, or that wait on something other than a ping: no
    // ping comes while they run.
    const QUIET: Duration = Duration::from_hours(1);

    // Signs `email` in, and gives the session's token.
    async fn sign_in(fixture: &Fixture, address: SocketAddr, email: &str) -> String {
        let (_, body) = test_support::sign_in(fixture, address, email).await;
        body["token"].as_str().unwrap().to_owned()
    }

    // A server for `fixture` that ada has signed in to, and her token.
    async fn signed_in(fixture: &Fixture) -> (Running, String) {
        let server = run(fixture.shared.clone()).await;
        let token = sign_in(fixture, server.address, "ada@example.com").await;
        (server, token)
    }

    fn w1(fixture: &Fixture) -> impl Future<Output = Workspace> {
        let db = fixture.shared.db.clone();
        async move { db.workspace(&workspace_id("w1")).await.unwrap() }
    }

    // Adds `email` to w1 as member `id`, who signs in and opens the workspace.
    async fn join(fixture: &Fixture, address: SocketAddr, id: &str, email: &str) {
        let workspace = w1(fixture).await;
        let address_of = Email::parse(email).unwrap();
        workspace
            .add_member(&member_id(id), &fresh_account(), &address_of)
            .await
            .unwrap();
        let token = sign_in(fixture, address, email).await;
        greeted(address, &token, 0).await;
    }

    // A feed of w1 for `token` after `since`, past its welcome.
    async fn greeted(address: SocketAddr, token: &str, since: u64) -> Client {
        let path = format!("/api/workspaces/w1/feed?since={since}");
        let mut client = open(address, &path, token).await.unwrap();
        say(&mut client, &["workspace.feed"]).await;
        let welcome = next(&mut client).await;
        assert!(matches!(welcome, Message::Welcome(_)), "{welcome:?}");
        client
    }

    fn joined(seq: u64, member: &str) -> Message {
        Message::Workspace(WorkspaceUpdate {
            workspace: WorkspaceId::try_from("w1".to_owned()).unwrap(),
            seq,
            change: WorkspaceChange::MemberJoined {
                member: MemberId::try_from(member.to_owned()).unwrap(),
            },
        })
    }

    #[tokio::test]
    async fn a_feed_keeps_its_place_among_the_connections() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = QUIET;
        let room = Arc::new(Semaphore::new(2));
        fixture.shared.connections = Arc::clone(&room);
        let (server, token) = signed_in(&fixture).await;
        let client = greeted(server.address, &token, 0).await;

        // One place is the feed's, the other is kept for the next connection.
        assert_eq!(room.available_permits(), 0);
        drop(client);
        drop(
            timeout(Duration::from_secs(10), room.acquire())
                .await
                .expect("the feed kept its place after it closed"),
        );
    }

    #[tokio::test]
    async fn a_member_gets_what_they_missed_then_each_change() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = QUIET;
        let (server, token) = signed_in(&fixture).await;
        let mut client = open(server.address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        say(&mut client, &["workspace.feed"]).await;
        let welcome = next(&mut client).await;
        let missed = next(&mut client).await;
        join(&fixture, server.address, "m2", "bo@example.com").await;
        let live = next(&mut client).await;

        let Message::Welcome(welcome) = welcome else {
            panic!("{welcome:?}");
        };
        assert_eq!(
            welcome.capabilities,
            [FEED, TASKS, DOCS].map(Capability::from)
        );
        assert_eq!(missed, joined(1, "m1"));
        assert_eq!(live, joined(2, "m2"));
    }

    #[tokio::test]
    async fn only_a_client_that_offers_tasks_hears_of_task_changes() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = QUIET;
        let (server, token) = signed_in(&fixture).await;
        let mut plain = greeted(server.address, &token, 0).await;
        let first = next(&mut plain).await;
        let task = Task::new(
            TaskId::try_from("t1".to_owned()).unwrap(),
            Title::try_from("Write".to_owned()).unwrap(),
            OrderKey::try_from("V".to_owned()).unwrap(),
        );
        let attribution = Attribution::by(member_id("m1"), 0);
        w1(&fixture)
            .await
            .add_task(&task, &attribution)
            .await
            .unwrap();
        fixture.shared.changed(&workspace_id("w1"));
        join(&fixture, server.address, "m2", "bo@example.com").await;
        let after_the_task = next(&mut plain).await;
        let path = "/api/workspaces/w1/feed";
        let mut both = open(server.address, path, &token).await.unwrap();
        say(&mut both, &["workspace.feed", "workspace.tasks"]).await;
        next(&mut both).await;

        let task_changed = Message::Workspace(WorkspaceUpdate {
            workspace: workspace_id("w1"),
            seq: 2,
            change: WorkspaceChange::TaskChanged {
                task: task.id,
                version: 1,
            },
        });
        assert_eq!((first, after_the_task), (joined(1, "m1"), joined(3, "m2")));
        assert_eq!(next(&mut both).await, joined(1, "m1"));
        assert_eq!(next(&mut both).await, task_changed);
        assert_eq!(next(&mut both).await, joined(3, "m2"));
    }

    #[tokio::test]
    async fn only_a_client_that_offers_docs_hears_of_doc_changes() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut plain = greeted(server.address, &token, 0).await;
        next(&mut plain).await;
        let doc = Doc::new(
            DocId::try_from("d1".to_owned()).unwrap(),
            DocText::new(
                Title::try_from("Framing".to_owned()).unwrap(),
                String::new(),
                None,
            )
            .unwrap(),
        );
        let attribution = Attribution::by(member_id("m1"), 0);
        w1(&fixture)
            .await
            .add_doc(&doc, &attribution)
            .await
            .unwrap();
        fixture.shared.changed(&workspace_id("w1"));
        join(&fixture, server.address, "m2", "bo@example.com").await;
        let after_the_doc = next(&mut plain).await;
        let path = "/api/workspaces/w1/feed";
        let mut docs = open(server.address, path, &token).await.unwrap();
        say(&mut docs, &["workspace.feed", "workspace.docs"]).await;
        next(&mut docs).await;

        let doc_changed = Message::Workspace(WorkspaceUpdate {
            workspace: workspace_id("w1"),
            seq: 2,
            change: WorkspaceChange::DocChanged {
                doc: doc.id,
                version: 1,
            },
        });
        assert_eq!(after_the_doc, joined(3, "m2"));
        assert_eq!(next(&mut docs).await, joined(1, "m1"));
        assert_eq!(next(&mut docs).await, doc_changed);
        assert_eq!(next(&mut docs).await, joined(3, "m2"));
    }

    #[tokio::test]
    async fn changes_arrive_once_and_in_order_whatever_wakes_the_feed() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = QUIET;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        join(&fixture, server.address, "m2", "bo@example.com").await;
        let other = WorkspaceId::try_from("w9".to_owned()).unwrap();
        fixture.shared.changes.send(other).unwrap();
        join(&fixture, server.address, "m3", "cy@example.com").await;
        let w1 = WorkspaceId::try_from("w1".to_owned()).unwrap();
        fixture.shared.changes.send(w1).unwrap();

        assert_eq!(next(&mut client).await, joined(2, "m2"));
        assert_eq!(next(&mut client).await, joined(3, "m3"));
        join(&fixture, server.address, "m4", "di@example.com").await;
        assert_eq!(next(&mut client).await, joined(4, "m4"));
    }

    #[tokio::test]
    async fn without_a_live_token_there_is_no_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let address = server.address;
        let refused = |result: Result<Client, tungstenite::Error>| match result {
            Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
            other => panic!("{:?}", other.map(|_| ())),
        };

        assert_eq!(
            refused(open(address, "/api/workspaces/w1/feed", "wrong").await),
            401
        );
        assert_eq!(
            refused(open(address, "/api/workspaces/w9/feed", &token).await),
            401
        );
        assert_eq!(
            refused(open(address, "/api/workspaces/w.1/feed", &token).await),
            400
        );
        // Signed in, but to an account that isn't a member.
        let bo = Email::parse("bo@example.com").unwrap();
        let db = &fixture.shared.db;
        db.add_account(&fresh_account(), &bo).await.unwrap();
        let stranger = sign_in(&fixture, address, "bo@example.com").await;
        assert_eq!(
            refused(open(address, "/api/workspaces/w1/feed", &stranger).await),
            401
        );
        fixture.advance(SESSION_LIFETIME);
        assert_eq!(
            refused(open(address, "/api/workspaces/w1/feed", &token).await),
            401
        );
    }

    #[tokio::test]
    async fn a_client_must_say_hello_and_offer_the_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let address = server.address;
        let mut wrong = open(address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        wrong
            .send(Wire::text("{\"id\":3,\"type\":\"welcome\"}"))
            .await
            .unwrap();
        let mut without = open(address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        say(&mut without, &[]).await;
        let mut older = open(address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        say_as(&mut older, PROTOCOL + 1, &["workspace.feed"]).await;

        assert_eq!(next(&mut wrong).await, error(code!("handshake.required")));
        assert_eq!(next(&mut without).await, error(code!("capability.missing")));
        assert!(ends(&mut without).await);
        assert_eq!(next(&mut older).await, error(code!("protocol.unsupported")));
        assert!(ends(&mut older).await);
    }

    #[tokio::test]
    async fn a_client_that_says_nothing_is_let_go() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut silent = open(server.address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();

        assert!(ends(&mut silent).await);
    }

    #[tokio::test]
    async fn an_oversized_message_ends_the_feed() {
        assert_eq!(MAX_MESSAGE, 16_384);
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = open(server.address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        client
            .send(Wire::text("x".repeat(MAX_MESSAGE + 1)))
            .await
            .unwrap();

        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_client_that_answers_pings_stays_until_its_session_ends() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        // Reading answers each ping; five of them outlast the two a silent client gets.
        for _ in 0..5 {
            let got = frame(&mut client).await;
            assert!(matches!(got, Some(Ok(Wire::Ping(_)))), "{got:?}");
        }
        fixture.advance(SESSION_LIFETIME);

        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_change_reaches_no_feed_whose_member_is_one_no_more() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = QUIET;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        w1(&fixture).await.unlink(&member_id("m1")).await;

        join(&fixture, server.address, "m2", "bo@example.com").await;

        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_member_whose_session_ends_during_a_page_gets_no_more_of_it() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = QUIET;
        // The clock reads as usual for `checks` more readings, then past every session.
        let checks = Arc::new(AtomicU64::new(u64::MAX));
        let (left, clock) = (Arc::clone(&checks), Arc::clone(&fixture.shared.clock));
        let past = Duration::from_secs(SESSION_LIFETIME.try_into().unwrap());
        fixture.shared.clock = Arc::new(Box::new(move || {
            let ended = left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err();
            clock() + past * u32::from(ended)
        }));
        let (server, token) = signed_in(&fixture).await;
        let w1 = w1(&fixture).await;
        let bo = Email::parse("bo@example.com").unwrap();
        w1.add_member(&member_id("m2"), &fresh_account(), &bo)
            .await
            .unwrap();
        w1.join(&member_id("m2")).await.unwrap();
        let mut client = open(server.address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        // A page of two changes, m2's and m1's: the session lasts through the first's check.
        checks.store(1, Ordering::SeqCst);
        say(&mut client, &["workspace.feed"]).await;

        assert!(matches!(next(&mut client).await, Message::Welcome(_)));
        assert_eq!(next(&mut client).await, joined(1, "m2"));
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_quiet_feed_ends_when_its_member_is_one_no_more() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        // Read meanwhile, so it's the unlinking that ends the feed, not unanswered pings.
        let unlink = async { w1(&fixture).await.unlink(&member_id("m1")).await };

        let ((), ended) = tokio::join!(unlink, ends(&mut client));
        assert!(ended);
        let again = open(server.address, "/api/workspaces/w1/feed", &token).await;
        again.unwrap_err();
    }

    #[tokio::test]
    async fn a_feed_ends_when_its_session_is_signed_out() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        let bearer = format!("authorization: Bearer {token}\r\n");

        let sign_out =
            test_support::request_with(server.address, "POST /api/sign-out", &bearer, "");

        let (_, ended) = tokio::join!(sign_out, ends(&mut client));
        assert!(ended);
    }

    #[tokio::test]
    async fn a_browsers_cookie_opens_a_feed_from_the_servers_own_page_only() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let open_from = async |origin: &str| {
            let mut request = format!("ws://{}/api/workspaces/w1/feed", server.address)
                .into_client_request()
                .unwrap();
            let cookie = format!("{SESSION_COOKIE}={token}");
            request
                .headers_mut()
                .insert("cookie", cookie.parse().unwrap());
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
            connect_async(request).await.map(|_| ())
        };

        let own = open_from(&format!("http://{}", server.address)).await;
        let other = open_from("http://other.example").await;

        own.unwrap();
        let Err(tungstenite::Error::Http(refused)) = other else {
            panic!("{other:?}");
        };
        assert_eq!(refused.status().as_u16(), 403);
    }

    #[tokio::test]
    async fn only_a_session_token_opens_a_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let upgrade = "connection: upgrade\r\nupgrade: websocket\r\nsec-websocket-version: 13\r\n\
                       sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n";
        let mut statuses = Vec::new();
        for authorization in [
            String::new(),
            "authorization: Bearer \r\n".to_owned(),
            format!("authorization: Basic {token}\r\n"),
            format!("authorization: bearer{token}\r\n"),
            format!("authorization: Bearer {token}\r\n"),
        ] {
            let headers = format!("{authorization}{upgrade}");
            let feed = "GET /api/workspaces/w1/feed";
            let (head, _) = test_support::request_with(server.address, feed, &headers, "").await;
            statuses.push(test_support::status(&head).to_owned());
        }
        let refused = "http/1.1 401 unauthorized";

        assert_eq!(
            statuses,
            [
                refused,
                refused,
                refused,
                refused,
                "http/1.1 101 switching protocols"
            ]
        );
    }

    #[tokio::test]
    async fn a_client_that_stops_answering_is_let_go() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let client = greeted(server.address, &token, 1).await;
        // Read below the WebSocket, so nothing answers the pings.
        let MaybeTlsStream::Plain(mut stream) = client.into_inner() else {
            panic!("not plain TCP");
        };
        let mut rest = Vec::new();

        timeout(Duration::from_secs(10), stream.read_to_end(&mut rest))
            .await
            .expect("the feed never let go")
            .unwrap();
        assert!(!rest.is_empty());
    }
}
