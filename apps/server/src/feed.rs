use std::time::Duration;

use axum::Extension;
use axum::extract::ws::{Message as Frame, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{Sink, SinkExt};
use serde::Deserialize;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::{Instant, MissedTickBehavior, interval_at, timeout};
use weacon_observability::Field;
use weacon_protocol::{Capability, Code, Envelope, ErrorBody, Message, WorkspaceId, code};

use crate::auth::Member;
use crate::connections::Place;
use crate::http::{Shared, failure};

/// What a client must offer, and the server offers, to get a workspace's changes.
const FEED: Code = code!("workspace.feed");
/// What the server offers to clients that read and change tasks.
const TASKS: Code = code!("workspace.tasks");

/// How long a feed waits for the client's hello, and how often it checks that the client
/// answers and its session hasn't ended; a send that takes longer than that ends the feed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub(crate) hello_within: Duration,
    pub(crate) ping_every: Duration,
}

pub(crate) const TIMING: Timing = Timing {
    hello_within: Duration::from_secs(10),
    ping_every: Duration::from_secs(30),
};
/// 16 KiB.
const MAX_MESSAGE: usize = 16_384;
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
        // Nobody listening is fine: feeds read what they missed from the database.
        Ok(true) => drop(shared.changes.send(member.workspace.id().clone())),
        Ok(false) => {}
        Err(fault) => return failure(&shared, fault),
    }
    let feed = Feed {
        _place: place,
        member,
        changes,
        last_seq: since,
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
}

impl Feed {
    /// Whether the feed may go on: its session is live and its account still a member.
    async fn allowed(&self, shared: &Shared) -> bool {
        let Member {
            workspace,
            token_hash,
            ..
        } = &self.member;
        match workspace.session_member(token_hash, shared.now()).await {
            Ok(member) => member.is_some(),
            Err(fault) => {
                shared
                    .log
                    .warn("feed failed", &[("err", Field::error(&fault))]);
                false
            }
        }
    }
}

async fn serve(mut socket: WebSocket, shared: Shared, mut feed: Feed) {
    let Timing {
        hello_within,
        ping_every,
    } = shared.feed;
    let greeted = timeout(hello_within, greet(&mut socket, &shared)).await;
    if !matches!(greeted, Ok(true)) || !catch_up(&mut socket, &shared, &mut feed).await {
        return;
    }
    let mut ping = interval_at(Instant::now() + ping_every, ping_every);
    // After a stall, one late tick rather than a burst that would count as missed answers.
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut unanswered: u8 = 0;
    loop {
        let open = tokio::select! {
            change = feed.changes.recv() => match change {
                Ok(id) if id != *feed.member.workspace.id() => true,
                // A feed that fell behind just reads more from the database.
                Ok(_) | Err(RecvError::Lagged(_)) => catch_up(&mut socket, &shared, &mut feed).await,
                Err(RecvError::Closed) => false,
            },
            _ = ping.tick() => {
                // Two pings unanswered: the client is gone, or its connection is.
                let open = unanswered < 2
                    && feed.allowed(&shared).await
                    && send(&mut socket, ping_every, Frame::Ping(Vec::new().into())).await;
                unanswered = unanswered.saturating_add(1);
                open
            },
            frame = socket.recv() => match frame {
                Some(Ok(Frame::Close(_)) | Err(_)) | None => false,
                Some(Ok(_)) => {
                    unanswered = 0;
                    true
                }
            },
        };
        if !open {
            return;
        }
    }
}

/// Sends every change after the last one sent, a page at a time, while its member still is one.
async fn catch_up(socket: &mut WebSocket, shared: &Shared, feed: &mut Feed) -> bool {
    loop {
        let page = match feed
            .member
            .workspace
            .changes_since(feed.last_seq, PAGE)
            .await
        {
            Ok(page) => page,
            Err(fault) => {
                shared
                    .log
                    .warn("feed failed", &[("err", Field::error(&fault))]);
                return false;
            }
        };
        let more = usize::try_from(PAGE).is_ok_and(|full| page.len() == full);
        for update in page {
            // Before each one, not each page: a member removed meanwhile gets nothing more.
            if !feed.allowed(shared).await {
                return false;
            }
            feed.last_seq = update.seq;
            if !reply(socket, shared, 0, Message::Workspace(update)).await {
                return false;
            }
        }
        if !more {
            return true;
        }
    }
}

/// Answers the client's hello; the feed goes on only for a client that offers it.
async fn greet(socket: &mut WebSocket, shared: &Shared) -> bool {
    let Some(Ok(Frame::Text(text))) = socket.recv().await else {
        return false;
    };
    let Ok(Envelope {
        id,
        message: Message::Hello(hello),
    }) = Envelope::from_json(text.as_bytes())
    else {
        reply(socket, shared, 0, error(code!("handshake.required"))).await;
        return false;
    };
    let feed = Capability::from(FEED);
    let answer = hello.answer(
        shared.app.clone(),
        vec![feed.clone(), Capability::from(TASKS)],
    );
    let welcomed = matches!(answer, Message::Welcome(_));
    if welcomed && !hello.capabilities.contains(&feed) {
        reply(socket, shared, id, error(code!("capability.missing"))).await;
        return false;
    }
    reply(socket, shared, id, answer).await && welcomed
}

fn error(code: Code) -> Message {
    Message::Error(ErrorBody {
        code,
        retryable: false,
    })
}

async fn reply(socket: &mut WebSocket, shared: &Shared, id: u32, message: Message) -> bool {
    let Ok(json) = (Envelope { id, message }).to_json() else {
        return false;
    };
    let Ok(text) = Utf8Bytes::try_from(json) else {
        return false;
    };
    send(socket, shared.feed.ping_every, Frame::Text(text)).await
}

/// Sends `frame`, or gives up after `within`: a client that stops reading can't hold the feed.
async fn send<S: Sink<Frame> + Unpin>(socket: &mut S, within: Duration, frame: Frame) -> bool {
    timeout(within, socket.send(frame))
        .await
        .is_ok_and(|sent| sent.is_ok())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use axum::http::header::AUTHORIZATION;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::io::AsyncReadExt;
    use tokio::sync::Semaphore;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::{self, Message as Wire, Utf8Bytes};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
    use weacon_protocol::{Hello, MemberId, PROTOCOL, WorkspaceChange, WorkspaceUpdate};

    use super::*;
    use crate::db::Workspace;
    use crate::email::Email;
    use crate::sign_in::SESSION_LIFETIME;
    use crate::test_support::{
        self, Fixture, Running, VERSION, fresh_account, member_id, run, workspace_id,
    };

    type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

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

    async fn open(
        address: SocketAddr,
        path: &str,
        token: &str,
    ) -> Result<Client, tungstenite::Error> {
        let mut request = format!("ws://{address}{path}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        connect_async(request).await.map(|(client, _)| client)
    }

    async fn say(client: &mut Client, capabilities: &[&str]) {
        say_as(client, PROTOCOL, capabilities).await;
    }

    async fn say_as(client: &mut Client, protocol: u32, capabilities: &[&str]) {
        let hello = Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol,
                app: weacon_appkit::build(&VERSION),
                capabilities: serde_json::from_value(json!(capabilities)).unwrap(),
            }),
        };
        let json = String::from_utf8(hello.to_json().unwrap()).unwrap();
        client.send(Wire::text(json)).await.unwrap();
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

    // The client's next frame; a feed that sends nothing for this long is broken.
    async fn frame(client: &mut Client) -> Option<Result<Wire, tungstenite::Error>> {
        timeout(Duration::from_secs(10), client.next())
            .await
            .expect("the feed went quiet")
    }

    // The client's next message, pings aside: they come every 100 ms.
    async fn next(client: &mut Client) -> Message {
        let text = timeout(Duration::from_secs(10), text(client))
            .await
            .expect("no message in time");
        Envelope::from_json(text.as_bytes()).unwrap().message
    }

    async fn text(client: &mut Client) -> Utf8Bytes {
        loop {
            match client.next().await {
                Some(Ok(Wire::Ping(_))) => {}
                Some(Ok(Wire::Text(text))) => return text,
                other => panic!("no message: {other:?}"),
            }
        }
    }

    // Whether the feed ends, pings aside, before it has had ten seconds to.
    async fn closed(client: &mut Client) -> bool {
        timeout(Duration::from_secs(10), ends(client))
            .await
            .unwrap_or(false)
    }

    async fn ends(client: &mut Client) -> bool {
        loop {
            match client.next().await {
                Some(Ok(Wire::Ping(_))) => {}
                None | Some(Ok(Wire::Close(_)) | Err(_)) => return true,
                Some(Ok(_)) => return false,
            }
        }
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
        let fixture = Fixture::new().await;
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
            [Capability::from(FEED), Capability::from(TASKS)]
        );
        assert_eq!(missed, joined(1, "m1"));
        assert_eq!(live, joined(2, "m2"));
    }

    #[tokio::test]
    async fn changes_arrive_once_and_in_order_whatever_wakes_the_feed() {
        let fixture = Fixture::new().await;
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
        assert!(closed(&mut without).await);
        assert_eq!(next(&mut older).await, error(code!("protocol.unsupported")));
        assert!(closed(&mut older).await);
    }

    #[tokio::test]
    async fn a_client_that_says_nothing_is_let_go() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut silent = open(server.address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();

        assert!(closed(&mut silent).await);
    }

    #[tokio::test]
    async fn an_oversized_message_ends_the_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = open(server.address, "/api/workspaces/w1/feed", &token)
            .await
            .unwrap();
        client
            .send(Wire::text("x".repeat(MAX_MESSAGE + 1)))
            .await
            .unwrap();

        assert!(closed(&mut client).await);
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

        assert!(closed(&mut client).await);
    }

    #[tokio::test]
    async fn a_change_reaches_no_feed_whose_member_is_one_no_more() {
        let mut fixture = Fixture::new().await;
        // Too long for a ping to end the feed first.
        fixture.shared.feed.ping_every = Duration::from_hours(1);
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        w1(&fixture).await.unlink(&member_id("m1")).await;

        join(&fixture, server.address, "m2", "bo@example.com").await;

        assert!(closed(&mut client).await);
    }

    #[tokio::test]
    async fn a_member_whose_session_ends_during_a_page_gets_no_more_of_it() {
        let mut fixture = Fixture::new().await;
        fixture.shared.feed.ping_every = Duration::from_hours(1);
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
        assert!(closed(&mut client).await);
    }

    #[tokio::test]
    async fn a_quiet_feed_ends_when_its_member_is_one_no_more() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        w1(&fixture).await.unlink(&member_id("m1")).await;

        assert!(closed(&mut client).await);
        let again = open(server.address, "/api/workspaces/w1/feed", &token).await;
        again.unwrap_err();
    }

    #[tokio::test]
    async fn only_a_bearer_token_opens_a_feed() {
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

    #[tokio::test(start_paused = true)]
    async fn a_send_the_client_never_takes_gives_up() {
        let mut stuck = futures_util::sink::unfold((), |(), _: Frame| {
            std::future::pending::<Result<(), std::convert::Infallible>>()
        });
        let mut taken = futures_util::sink::drain();
        let frame = || Frame::Ping(Vec::new().into());

        assert!(!send(&mut stuck, TIMING.ping_every, frame()).await);
        assert!(send(&mut taken, TIMING.ping_every, frame()).await);
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
