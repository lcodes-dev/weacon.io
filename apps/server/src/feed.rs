use std::convert::Infallible;

use axum::Extension;
use axum::Router;
use axum::extract::ws::{Message as Frame, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::sync::mpsc;
use tokio::time::timeout;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    Asker, Capability, Code, Envelope, Message, WorkspaceChange, WorkspaceId, code,
};

use crate::auth::{Member, Session};
use crate::connections::Place;
use crate::http::failure;
use crate::relay::{self, Ask, Refusal, SETTINGS};
use crate::shared::Shared;
use crate::socket::{Closed, Pings, greet, reply};

/// What a client must offer, and the server offers, to get a workspace's changes.
const FEED: Code = code!("workspace.feed");
/// What the server offers to clients that read and change tasks.
const TASKS: Code = code!("workspace.tasks");
/// What the server offers to clients that read and revise docs.
const DOCS: Code = code!("workspace.docs");
/// How many answers from machines wait for the client to take them.
const ANSWERS: usize = 16;

const MAX_MESSAGE: usize = 16 * 1024;
const PAGE: u32 = 500;

#[derive(Deserialize)]
pub(crate) struct Since {
    #[serde(default)]
    since: u64,
}

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/workspaces/{workspace}/feed", get(feed))
}

/// `GET /api/workspaces/{workspace}/feed`: a member's WebSocket of the workspace's changes
/// after `since`, then each new one as it happens.
pub(crate) async fn feed(
    State(shared): State<Shared>,
    Query(Since { since }): Query<Since>,
    Extension(place): Extension<Place>,
    (member, session): (Member, Session),
    upgrade: WebSocketUpgrade,
) -> Response {
    // Subscribed before the backlog is read, so no change falls between the two.
    let changes = shared.changes.subscribe();
    // A member joins a workspace when they first open it.
    match member.workspace.join(&member.id).await {
        Ok(true) => shared.wake_feeds(member.workspace.id()),
        Ok(false) => {}
        Err(fault) => return failure(fault),
    }
    let asker = Asker {
        account: session.account,
        member: member.id.clone(),
    };
    let (answer_to, answers) = mpsc::channel(ANSWERS);
    let feed = Feed {
        _place: place,
        member,
        asker,
        changes,
        last_seq: since,
        offers: Offers::default(),
        answer_to,
        answers,
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
    /// Who the feed's client is, for the messages it relays to its machines.
    asker: Asker,
    /// Which workspace changed; the changes themselves are read from the database, in order.
    changes: Receiver<WorkspaceId>,
    last_seq: u64,
    offers: Offers,
    /// Machines' answers to what the client asked them through the relay.
    answer_to: mpsc::Sender<Envelope>,
    answers: mpsc::Receiver<Envelope>,
}

/// Which features the client offered besides the feed; it hears only of their changes.
#[derive(Default)]
struct Offers {
    tasks: bool,
    docs: bool,
    settings: bool,
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
    let [feed_offer, tasks, docs, settings] = [FEED, TASKS, DOCS, SETTINGS].map(Capability::new);
    let offered = vec![
        feed_offer.clone(),
        tasks.clone(),
        docs.clone(),
        settings.clone(),
    ];
    let greeted = greet(socket, shared, offered, &feed_offer);
    let hello = timeout(shared.sockets.hello_within, greeted)
        .await
        .map_err(|_| Closed)??;
    feed.offers = Offers {
        tasks: hello.capabilities.contains(&tasks),
        docs: hello.capabilities.contains(&docs),
        settings: hello.capabilities.contains(&settings),
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
            Some(answer) = feed.answers.recv() => {
                feed.allowed(shared).await?;
                reply(socket, shared, answer.id, answer.message).await?;
            }
            frame = socket.recv() => {
                let frame = frame.ok_or(Closed)?;
                pings.heard(&frame)?;
                if let Ok(Frame::Text(text)) = &frame
                    && let Ok(envelope) = Envelope::from_json(text.as_bytes())
                {
                    received(socket, shared, feed, envelope).await?;
                }
            }
        }
    }
}

/// Gives what the client sent to the feature it is for, if the client offered that feature;
/// anything else is dropped.
async fn received(
    socket: &mut WebSocket,
    shared: &Shared,
    feed: &Feed,
    envelope: Envelope,
) -> Result<(), Closed> {
    if feed.offers.settings && relay::carries(&envelope.message) {
        return relay(socket, shared, feed, envelope).await;
    }
    Ok(())
}

/// Relays a message the client sent for one of its machines; the machine answers later, and a
/// refusal answers at once.
async fn relay(
    socket: &mut WebSocket,
    shared: &Shared,
    feed: &Feed,
    envelope: Envelope,
) -> Result<(), Closed> {
    feed.allowed(shared).await?;
    let id = envelope.id;
    let workspace = feed.member.workspace.id();
    let ask = Ask {
        asker: &feed.asker,
        workspace,
    };
    match relay::forward(shared, &ask, envelope, &feed.answer_to).await {
        Ok(()) => Ok(()),
        Err(Refusal::Refused(refusal)) => reply(socket, shared, id, Message::Error(refusal)).await,
        Err(Refusal::Failed(fault)) => Err(feed_failed(shared, workspace, &fault)),
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
                // No feed offers `workspace.chats` or `workspace.proposals` yet.
                WorkspaceChange::ChatChanged { .. } | WorkspaceChange::ProposalChanged { .. } => {
                    false
                }
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
    use tokio_tungstenite::tungstenite::Message as Wire;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::{MaybeTlsStream, connect_async};
    use weacon_protocol::{MemberId, PROTOCOL, WorkspaceChange, WorkspaceUpdate};

    use weacon_model::{Attribution, Doc, DocText, OrderKey, Task, Title};
    use weacon_protocol::{DocId, TaskId};

    use super::*;
    use crate::auth::SESSION_COOKIE;
    use crate::db::Workspace;
    use crate::sign_in::SESSION_LIFETIME;
    use crate::socket::refusal;
    use crate::test_support::socket::{
        Socket, ends, frame, next, open, say, say_as, upgrade_status,
    };
    use crate::test_support::{self, Fixture, PATIENCE, fresh_account, member_id, workspace_id};

    fn w1(fixture: &Fixture) -> impl Future<Output = Workspace> {
        let db = fixture.shared.db.clone();
        async move { db.workspace(&workspace_id("w1")).await.unwrap() }
    }

    // Adds `email` to w1 as member `id`, who signs in and opens the workspace.
    async fn join(fixture: &Fixture, address: SocketAddr, id: &str, email: &str) {
        let workspace = w1(fixture).await;
        workspace
            .add_member(
                &member_id(id),
                &fresh_account(),
                &test_support::email(email),
            )
            .await
            .unwrap();
        let member = fixture.sign_in_client(address, email).await;
        greeted(address, &member.token, 0).await;
    }

    // A feed of w1 for `token` after `since`, past its welcome.
    async fn greeted(address: SocketAddr, token: &str, since: u64) -> Socket {
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
        fixture.quiet();
        let room = Arc::new(Semaphore::new(2));
        fixture.shared.connections = Arc::clone(&room);
        let (server, ada) = fixture.client("ada@example.com").await;
        let client = greeted(server.address, &ada.token, 0).await;

        // One place is the feed's, the other is kept for the next connection.
        assert_eq!(room.available_permits(), 0);
        drop(client);
        drop(
            timeout(PATIENCE, room.acquire())
                .await
                .expect("the feed kept its place after it closed"),
        );
    }

    #[tokio::test]
    async fn a_member_gets_what_they_missed_then_each_change() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = open(server.address, "/api/workspaces/w1/feed", &ada.token)
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
            [FEED, TASKS, DOCS, SETTINGS].map(Capability::new)
        );
        assert_eq!(missed, joined(1, "m1"));
        assert_eq!(live, joined(2, "m2"));
    }

    #[tokio::test]
    async fn only_a_client_that_offers_tasks_hears_of_task_changes() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut plain = greeted(server.address, &ada.token, 0).await;
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
        fixture.shared.wake_feeds(&workspace_id("w1"));
        join(&fixture, server.address, "m2", "bo@example.com").await;
        let after_the_task = next(&mut plain).await;
        let path = "/api/workspaces/w1/feed";
        let mut both = open(server.address, path, &ada.token).await.unwrap();
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
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut plain = greeted(server.address, &ada.token, 0).await;
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
        fixture.shared.wake_feeds(&workspace_id("w1"));
        join(&fixture, server.address, "m2", "bo@example.com").await;
        let after_the_doc = next(&mut plain).await;
        let path = "/api/workspaces/w1/feed";
        let mut docs = open(server.address, path, &ada.token).await.unwrap();
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
        fixture.quiet();
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = greeted(server.address, &ada.token, 1).await;
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
        let (server, ada) = fixture.client("ada@example.com").await;
        let address = server.address;

        assert_eq!(
            upgrade_status(open(address, "/api/workspaces/w1/feed", "wrong").await),
            401
        );
        assert_eq!(
            upgrade_status(open(address, "/api/workspaces/w9/feed", &ada.token).await),
            401
        );
        assert_eq!(
            upgrade_status(open(address, "/api/workspaces/w.1/feed", &ada.token).await),
            400
        );
        // Signed in, but to an account that isn't a member.
        let bo = test_support::email("bo@example.com");
        let db = &fixture.shared.db;
        db.add_account(&fresh_account(), &bo).await.unwrap();
        let stranger = fixture.sign_in_client(address, "bo@example.com").await;
        assert_eq!(
            upgrade_status(open(address, "/api/workspaces/w1/feed", &stranger.token).await),
            401
        );
        fixture.advance(SESSION_LIFETIME);
        assert_eq!(
            upgrade_status(open(address, "/api/workspaces/w1/feed", &ada.token).await),
            401
        );
    }

    #[tokio::test]
    async fn a_client_must_say_hello_and_offer_the_feed() {
        let fixture = Fixture::new().await;
        let (server, ada) = fixture.client("ada@example.com").await;
        let address = server.address;
        let mut wrong = open(address, "/api/workspaces/w1/feed", &ada.token)
            .await
            .unwrap();
        wrong
            .send(Wire::text("{\"id\":3,\"type\":\"welcome\"}"))
            .await
            .unwrap();
        let mut without = open(address, "/api/workspaces/w1/feed", &ada.token)
            .await
            .unwrap();
        say(&mut without, &[]).await;
        let mut older = open(address, "/api/workspaces/w1/feed", &ada.token)
            .await
            .unwrap();
        say_as(&mut older, PROTOCOL + 1, &["workspace.feed"]).await;

        assert_eq!(next(&mut wrong).await, refusal(code!("handshake.required")));
        assert_eq!(
            next(&mut without).await,
            refusal(code!("capability.missing"))
        );
        assert!(ends(&mut without).await);
        assert_eq!(
            next(&mut older).await,
            refusal(code!("protocol.unsupported"))
        );
        assert!(ends(&mut older).await);
    }

    #[tokio::test]
    async fn a_client_that_says_nothing_is_let_go() {
        let fixture = Fixture::new().await;
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut silent = open(server.address, "/api/workspaces/w1/feed", &ada.token)
            .await
            .unwrap();

        assert!(ends(&mut silent).await);
    }

    #[tokio::test]
    async fn an_oversized_message_ends_the_feed() {
        assert_eq!(MAX_MESSAGE, 16_384);
        let fixture = Fixture::new().await;
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = open(server.address, "/api/workspaces/w1/feed", &ada.token)
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
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = greeted(server.address, &ada.token, 1).await;
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
        fixture.quiet();
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = greeted(server.address, &ada.token, 1).await;
        w1(&fixture).await.unlink(&member_id("m1")).await;

        join(&fixture, server.address, "m2", "bo@example.com").await;

        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_member_whose_session_ends_during_a_page_gets_no_more_of_it() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();
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
        let (server, ada) = fixture.client("ada@example.com").await;
        let w1 = w1(&fixture).await;
        let bo = test_support::email("bo@example.com");
        w1.add_member(&member_id("m2"), &fresh_account(), &bo)
            .await
            .unwrap();
        w1.join(&member_id("m2")).await.unwrap();
        let mut client = open(server.address, "/api/workspaces/w1/feed", &ada.token)
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
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = greeted(server.address, &ada.token, 1).await;
        // Read meanwhile, so it's the unlinking that ends the feed, not unanswered pings.
        let unlink = async { w1(&fixture).await.unlink(&member_id("m1")).await };

        let ((), ended) = tokio::join!(unlink, ends(&mut client));
        assert!(ended);
        let again = open(server.address, "/api/workspaces/w1/feed", &ada.token).await;
        again.unwrap_err();
    }

    #[tokio::test]
    async fn a_feed_ends_when_its_session_is_signed_out() {
        let fixture = Fixture::new().await;
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = greeted(server.address, &ada.token, 1).await;

        let sign_out = ada.send("POST /api/sign-out", "");

        let (_, ended) = tokio::join!(sign_out, ends(&mut client));
        assert!(ended);
    }

    #[tokio::test]
    async fn a_browsers_cookie_opens_a_feed_from_the_servers_own_page_only() {
        let fixture = Fixture::new().await;
        let (server, ada) = fixture.client("ada@example.com").await;
        let open_from = async |origin: &str| {
            let mut request = format!("ws://{}/api/workspaces/w1/feed", server.address)
                .into_client_request()
                .unwrap();
            let cookie = format!("{SESSION_COOKIE}={}", ada.token);
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
        assert_eq!(upgrade_status(other), 403);
    }

    #[tokio::test]
    async fn only_a_session_token_opens_a_feed() {
        let fixture = Fixture::new().await;
        let (server, ada) = fixture.client("ada@example.com").await;
        let upgrade = "connection: upgrade\r\nupgrade: websocket\r\nsec-websocket-version: 13\r\n\
                       sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n";
        let mut statuses = Vec::new();
        for authorization in [
            String::new(),
            "authorization: Bearer \r\n".to_owned(),
            format!("authorization: Basic {}\r\n", ada.token),
            format!("authorization: bearer{}\r\n", ada.token),
            format!("authorization: Bearer {}\r\n", ada.token),
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
        let (server, ada) = fixture.client("ada@example.com").await;
        let client = greeted(server.address, &ada.token, 1).await;
        // Read below the WebSocket, so nothing answers the pings.
        let MaybeTlsStream::Plain(mut stream) = client.into_inner() else {
            panic!("not plain TCP");
        };
        let mut rest = Vec::new();

        timeout(PATIENCE, stream.read_to_end(&mut rest))
            .await
            .expect("the feed never let go")
            .unwrap();
        assert!(!rest.is_empty());
    }
}
