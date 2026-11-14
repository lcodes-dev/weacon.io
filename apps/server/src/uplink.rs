//! A paired machine's connection, which its core dials out to and keeps open.

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{Message as Frame, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::timeout;
use weacon_observability::{Field, Logger};
use weacon_protocol::{Capability, Code, MachineId, code};

use crate::auth::Machine;
use crate::connections::Place;
use crate::http::Shared;
use crate::socket::{MAX_MESSAGE, Pings, error, greet, reply, send};

/// What a core must offer, and the server offers, to keep a machine's connection.
const CONNECT: Code = code!("machine.connect");

/// `GET /api/machines/connect`: a paired machine's WebSocket, open until the machine is
/// unpaired, its core goes, or the server stops.
pub(crate) async fn connect(
    State(shared): State<Shared>,
    Extension(place): Extension<Place>,
    machine: Machine,
    upgrade: WebSocketUpgrade,
) -> Response {
    let unpaired = shared.unpaired.subscribe();
    let log = shared
        .log
        .child(&[("machine_id", machine.id.as_str().into())]);
    let link = Link {
        _place: place,
        machine,
        unpaired,
        log,
        gone: false,
    };
    upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve(socket, shared, link))
}

struct Link {
    _place: Place,
    machine: Machine,
    unpaired: Receiver<MachineId>,
    log: Logger,
    /// Set once the machine is found unpaired, and not when it couldn't be looked up.
    gone: bool,
}

impl Link {
    /// Whether the machine is still paired; `false` too when that can't be looked up.
    async fn paired(&mut self, shared: &Shared) -> bool {
        match shared.db.machine_with(&self.machine.token_hash).await {
            Ok(found) => {
                self.gone = found.is_none();
                !self.gone
            }
            Err(fault) => {
                self.log
                    .warn("machine link failed", &[("err", Field::error(&fault))]);
                false
            }
        }
    }
}

async fn serve(mut socket: WebSocket, shared: Shared, mut link: Link) {
    let offer = Capability::from(CONNECT);
    let greeted = greet(&mut socket, &shared, vec![offer.clone()], &offer);
    if !matches!(
        timeout(shared.sockets.hello_within, greeted).await,
        Ok(Some(_))
    ) {
        return;
    }
    // It may have been unpaired since it was let in, before it could hear of it.
    let mut open = link.paired(&shared).await;
    if open {
        link.log.info("machine connected", &[]);
    }
    let mut pings = Pings::new(shared.sockets.ping_every);
    while open {
        open = tokio::select! {
            unpaired = link.unpaired.recv() => match unpaired {
                Ok(id) if id != link.machine.id => true,
                // A notice missed is checked against the database.
                Ok(_) | Err(RecvError::Lagged(_)) => link.paired(&shared).await,
                Err(RecvError::Closed) => false,
            },
            () = pings.due() => pings.ping(&mut socket, link.paired(&shared)).await,
            frame = socket.recv() => pings.heard(frame.as_ref()),
        };
    }
    // A core that is gone just doesn't hear it.
    if link.gone {
        reply(&mut socket, &shared, 0, error(code!("machine.unpaired"))).await;
    }
    send(&mut socket, shared.sockets.ping_every, Frame::Close(None)).await;
    link.log.info("machine disconnected", &[]);
}
#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::Value;
    use tokio_tungstenite::tungstenite::{self, Message as Wire};
    use weacon_protocol::{AccountId, ErrorBody, Message, Welcome};

    use super::*;
    use crate::test_support::socket::{Client, ends, next, open, say};
    use crate::test_support::{Fixture, Paired, ask, pair_machine, run};

    const CONNECT_PATH: &str = "/api/machines/connect";

    // A paired machine's connection, welcomed: the header proving ada's sign-in, her session's
    // token, the machine, its token and the connection.
    async fn connected(address: SocketAddr, fixture: &Fixture) -> (Paired, Client) {
        let paired = pair_machine(fixture, address).await;
        let mut client = open(address, CONNECT_PATH, &paired.token).await.unwrap();
        say(&mut client, &["machine.connect"]).await;
        let welcome = next(&mut client).await;
        assert!(
            matches!(&welcome, Message::Welcome(Welcome { capabilities, .. })
                if *capabilities == [Capability::from(CONNECT)]),
            "{welcome:?}"
        );
        (paired, client)
    }

    fn code_of(message: &Message) -> Option<&str> {
        let Message::Error(ErrorBody { code, .. }) = message else {
            return None;
        };
        Some(code.as_str())
    }

    fn refused_with(error: tungstenite::Error) -> u16 {
        let tungstenite::Error::Http(response) = error else {
            panic!("{error}");
        };
        response.status().as_u16()
    }

    async fn past_pings(client: &mut Client) -> Option<Result<Wire, tungstenite::Error>> {
        loop {
            let frame = client.next().await;
            if !matches!(frame, Some(Ok(Wire::Ping(_)))) {
                return frame;
            }
        }
    }

    fn id_of(machine: &Value) -> MachineId {
        MachineId::try_from(machine["id"].as_str().unwrap().to_owned()).unwrap()
    }

    #[tokio::test]
    async fn only_a_paired_machines_token_opens_a_connection() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (paired, _client) = connected(server.address, &fixture).await;

        for token in ["", "nonsense", paired.session.as_str()] {
            let refused = open(server.address, CONNECT_PATH, token).await.unwrap_err();
            assert_eq!(refused_with(refused), 401, "{token}");
        }
    }

    #[tokio::test]
    async fn a_core_must_offer_the_connection() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let paired = pair_machine(&fixture, server.address).await;
        let mut client = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap();

        say(&mut client, &["chat.send"]).await;

        assert_eq!(
            code_of(&next(&mut client).await),
            Some("capability.missing")
        );
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn unpairing_a_machine_ends_its_connection_at_once() {
        let fixture = Fixture::new().await;
        // Pings far apart, so only the notice can end it in time.
        let mut shared = fixture.shared.clone();
        shared.sockets.ping_every = Duration::from_hours(1);
        let server = run(shared).await;
        let (paired, mut client) = connected(server.address, &fixture).await;
        let path = format!(
            "DELETE /api/machines/{}",
            paired.machine["id"].as_str().unwrap()
        );

        let (unpaired, _) = ask(server.address, &path, &paired.proof, "").await;
        let said = next(&mut client).await;
        let ended = ends(&mut client).await;
        let again = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap_err();

        assert_eq!(unpaired, "http/1.1 204 no content");
        assert_eq!(code_of(&said), Some("machine.unpaired"));
        assert!(ended);
        assert_eq!(refused_with(again), 401);
    }

    #[tokio::test]
    async fn a_connection_ends_at_its_next_ping_once_the_machine_is_gone() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (paired, mut client) = connected(server.address, &fixture).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();

        // Unpaired without a notice, as by another server.
        let db = &fixture.shared.db;
        assert!(db.unpair(&account, &id_of(&paired.machine)).await.unwrap());

        assert_eq!(code_of(&next(&mut client).await), Some("machine.unpaired"));
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_paired_machines_connection_outlasts_its_pings() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, mut client) = connected(server.address, &fixture).await;
        // Reading answers each ping; the fixture pings every 100 ms.
        for _ in 0..4 {
            assert!(matches!(client.next().await, Some(Ok(Wire::Ping(_)))));
        }
        client.send(Wire::Ping(Vec::new().into())).await.unwrap();

        let answered = past_pings(&mut client).await;

        assert!(matches!(answered, Some(Ok(Wire::Pong(_)))), "{answered:?}");
    }

    #[tokio::test]
    async fn a_machine_unpaired_before_its_hello_is_told_so() {
        let fixture = Fixture::new().await;
        // Pings far apart, so only the check after the hello can find it unpaired in time.
        let mut shared = fixture.shared.clone();
        shared.sockets.ping_every = Duration::from_hours(1);
        let server = run(shared).await;
        let paired = pair_machine(&fixture, server.address).await;
        let mut client = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap();
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();
        let db = &fixture.shared.db;
        assert!(db.unpair(&account, &id_of(&paired.machine)).await.unwrap());

        say(&mut client, &["machine.connect"]).await;

        assert!(matches!(next(&mut client).await, Message::Welcome(_)));
        assert_eq!(code_of(&next(&mut client).await), Some("machine.unpaired"));
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn another_machines_unpairing_leaves_a_connection_open() {
        let fixture = Fixture::new().await;
        let mut shared = fixture.shared.clone();
        shared.sockets.ping_every = Duration::from_hours(1);
        let server = run(shared).await;
        let (_, mut client) = connected(server.address, &fixture).await;
        let other = MachineId::try_from("other".to_owned()).unwrap();

        fixture.shared.unpaired.send(other).unwrap();
        client.send(Wire::Ping(Vec::new().into())).await.unwrap();

        assert!(matches!(client.next().await, Some(Ok(Wire::Pong(_)))));
    }
}
