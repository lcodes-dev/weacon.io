use std::time::Duration;

use axum::extract::ws::{Message as Frame, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::response::Response;
use futures_util::{Sink, SinkExt};
use serde::Deserialize;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::{Instant, MissedTickBehavior, interval_at, timeout};
use weacon_observability::{Fault, Field};
use weacon_protocol::{Capability, Code, Envelope, ErrorBody, Message, WorkspaceId, code};

use crate::db::Workspace;
use crate::http::{Shared, failure, refused};
use crate::sign_in::hash;

/// What a client must offer, and the server offers, to get a workspace's changes.
const FEED: Code = code!("workspace.feed");

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

/// `GET /workspaces/{workspace}/feed`: a member's WebSocket of the workspace's changes after
/// `since`, then each new one as it happens.
pub(crate) async fn feed(
    State(shared): State<Shared>,
    Path(workspace): Path<WorkspaceId>,
    Query(Since { since }): Query<Since>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(token_hash) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(hash)
    else {
        return refused(code!("sign_in.required"));
    };
    match member_workspace(&shared, &workspace, &token_hash).await {
        Ok(Some((workspace, ends_at))) => {
            // Subscribed before the backlog is read, so no change falls between the two.
            let changes = shared.changes.subscribe();
            let feed = Feed {
                workspace,
                ends_at,
                changes,
                last_seq: since,
            };
            upgrade
                .max_message_size(MAX_MESSAGE)
                .max_frame_size(MAX_MESSAGE)
                .on_upgrade(move |socket| serve(socket, shared, feed))
        }
        Ok(None) => refused(code!("sign_in.required")),
        Err(fault) => failure(&shared, fault),
    }
}

/// The workspace `id`, and when the session hashed as `token_hash` ends, if it is live there.
async fn member_workspace(
    shared: &Shared,
    id: &WorkspaceId,
    token_hash: &[u8],
) -> Result<Option<(Workspace, i64)>, Fault> {
    let Some(workspace) = shared.db.find_workspace(id).await? else {
        return Ok(None);
    };
    let ends_at = workspace.session_end(token_hash, shared.now()).await?;
    Ok(ends_at.map(|ends_at| (workspace, ends_at)))
}

struct Feed {
    workspace: Workspace,
    /// When the session the feed was opened with ends, in seconds since the Unix epoch.
    ends_at: i64,
    /// Which workspace changed; the changes themselves are read from the database, in order.
    changes: Receiver<WorkspaceId>,
    last_seq: u64,
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
                Ok(id) if id != *feed.workspace.id() => true,
                // A feed that fell behind just reads more from the database.
                Ok(_) | Err(RecvError::Lagged(_)) => catch_up(&mut socket, &shared, &mut feed).await,
                Err(RecvError::Closed) => false,
            },
            _ = ping.tick() => {
                // Two pings unanswered: the client is gone, or its connection is.
                let open = unanswered < 2
                    && shared.now() < feed.ends_at
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

/// Sends every change after the last one sent, a page at a time.
async fn catch_up(socket: &mut WebSocket, shared: &Shared, feed: &mut Feed) -> bool {
    loop {
        let page = match feed.workspace.changes_since(feed.last_seq, PAGE).await {
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
    let answer = hello.answer(shared.app.clone(), vec![feed.clone()]);
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

    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::{self, Message as Wire, Utf8Bytes};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
    use weacon_protocol::{Hello, MemberId, PROTOCOL, WorkspaceChange, WorkspaceUpdate};

    use super::*;
    use crate::email::Email;
    use crate::sign_in::SESSION_LIFETIME;
    use crate::test_support::{
        Fixture, Running, VERSION, member_id, request, run, start_sign_in, workspace_id,
    };

    type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    // Signs `email` in to w1, and gives the session's token.
    async fn sign_in(fixture: &Fixture, address: SocketAddr, email: &str) -> String {
        let (_, challenge) = start_sign_in(address, email).await;
        let code = fixture.letter().await.1;
        let finish =
            json!({ "workspace": "w1", "email": email, "challenge": challenge, "code": code });
        let (_, body) = request(address, "POST /sign-in/finish", &finish.to_string()).await;
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    // A server for `fixture` that ada has signed in to, and her token.
    async fn signed_in(fixture: &Fixture) -> (Running, String) {
        let server = run(fixture.shared.clone()).await;
        let token = sign_in(fixture, server.address, "ada@example.com").await;
        (server, token)
    }

    async fn add_member(fixture: &Fixture, id: &str, email: &str) {
        let workspace = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let email = Email::parse(email).unwrap();
        workspace.add_member(&member_id(id), &email).await.unwrap();
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
        let path = format!("/workspaces/w1/feed?since={since}");
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
    async fn a_member_gets_what_they_missed_then_each_change() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = open(server.address, "/workspaces/w1/feed", &token)
            .await
            .unwrap();
        say(&mut client, &["workspace.feed"]).await;
        let welcome = next(&mut client).await;
        let missed = next(&mut client).await;
        add_member(&fixture, "m2", "bo@example.com").await;
        sign_in(&fixture, server.address, "bo@example.com").await;
        let live = next(&mut client).await;

        let Message::Welcome(welcome) = welcome else {
            panic!("{welcome:?}");
        };
        assert_eq!(welcome.capabilities, [Capability::from(FEED)]);
        assert_eq!(missed, joined(1, "m1"));
        assert_eq!(live, joined(2, "m2"));
    }

    #[tokio::test]
    async fn changes_arrive_once_and_in_order_whatever_wakes_the_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = greeted(server.address, &token, 1).await;
        add_member(&fixture, "m2", "bo@example.com").await;
        add_member(&fixture, "m3", "cy@example.com").await;
        let other = WorkspaceId::try_from("w9".to_owned()).unwrap();
        fixture.shared.changes.send(other).unwrap();
        sign_in(&fixture, server.address, "bo@example.com").await;
        sign_in(&fixture, server.address, "cy@example.com").await;
        let w1 = WorkspaceId::try_from("w1".to_owned()).unwrap();
        fixture.shared.changes.send(w1).unwrap();

        assert_eq!(next(&mut client).await, joined(2, "m2"));
        assert_eq!(next(&mut client).await, joined(3, "m3"));
        add_member(&fixture, "m4", "di@example.com").await;
        sign_in(&fixture, server.address, "di@example.com").await;
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
            refused(open(address, "/workspaces/w1/feed", "wrong").await),
            401
        );
        assert_eq!(
            refused(open(address, "/workspaces/w9/feed", &token).await),
            401
        );
        assert_eq!(
            refused(open(address, "/workspaces/w.1/feed", &token).await),
            400
        );
        fixture.advance(SESSION_LIFETIME);
        assert_eq!(
            refused(open(address, "/workspaces/w1/feed", &token).await),
            401
        );
    }

    #[tokio::test]
    async fn a_client_must_say_hello_and_offer_the_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let address = server.address;
        let mut wrong = open(address, "/workspaces/w1/feed", &token).await.unwrap();
        wrong
            .send(Wire::text("{\"id\":3,\"type\":\"welcome\"}"))
            .await
            .unwrap();
        let mut without = open(address, "/workspaces/w1/feed", &token).await.unwrap();
        say(&mut without, &[]).await;
        let mut older = open(address, "/workspaces/w1/feed", &token).await.unwrap();
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
        let mut silent = open(server.address, "/workspaces/w1/feed", &token)
            .await
            .unwrap();

        assert!(closed(&mut silent).await);
    }

    #[tokio::test]
    async fn an_oversized_message_ends_the_feed() {
        let fixture = Fixture::new().await;
        let (server, token) = signed_in(&fixture).await;
        let mut client = open(server.address, "/workspaces/w1/feed", &token)
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
