//! A paired machine's connection, which its core dials out to and keeps open.

use std::convert::Infallible;

use axum::Extension;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as Frame, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::timeout;
use weacon_observability::{Field, Logger};
use weacon_protocol::{Capability, Code, Envelope, MAX_FRAME, MachineId, code};

use crate::auth::Machine;
use crate::connections::Place;
use crate::links::Connected;
use crate::relay::RELAYED;
use crate::shared::Shared;
use crate::socket::{Closed, Pings, greet, refusal, reply, send};

/// What a core must offer, and the server offers, to keep a machine's connection.
const CONNECT: Code = code!("machine.connect");

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/machines/connect", get(connect))
}

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
        connected: false,
    };
    upgrade
        // Chat events travel on it, so it takes any message the protocol allows.
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| serve(socket, shared, link))
}

struct Link {
    _place: Place,
    machine: Machine,
    unpaired: Receiver<MachineId>,
    log: Logger,
    /// Set once the machine is found unpaired, and not when it couldn't be looked up.
    gone: bool,
    connected: bool,
}

impl Link {
    /// Goes on only while the machine is still paired; a lookup that fails ends the
    /// connection too, without telling the core it was unpaired.
    async fn paired(&mut self, shared: &Shared) -> Result<(), Closed> {
        match shared.db.machine_with(&self.machine.token_hash).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                self.gone = true;
                Err(Closed)
            }
            Err(fault) => {
                self.log
                    .warn("machine link failed", &[("err", Field::error(&fault))]);
                Err(Closed)
            }
        }
    }
}

async fn serve(mut socket: WebSocket, shared: Shared, mut link: Link) {
    let Err(Closed) = run(&mut socket, &shared, &mut link).await;
    // A core that is gone just doesn't hear it.
    if link.gone {
        drop(reply(&mut socket, &shared, 0, refusal(code!("machine.unpaired"))).await);
    }
    drop(send(&mut socket, shared.sockets.ping_every, Frame::Close(None)).await);
    if link.connected {
        link.log.info("machine disconnected", &[]);
    }
}

async fn run(
    socket: &mut WebSocket,
    shared: &Shared,
    link: &mut Link,
) -> Result<Infallible, Closed> {
    let offer = Capability::from(CONNECT);
    let offers = std::iter::once(offer.clone())
        .chain(RELAYED.map(Capability::from))
        .collect();
    let greeted = greet(socket, shared, offers, &offer);
    let hello = timeout(shared.sockets.hello_within, greeted)
        .await
        .map_err(|_| Closed)??;
    // It may have been unpaired since it was let in, before it could hear of it.
    link.paired(shared).await?;
    link.connected = true;
    link.log.info("machine connected", &[]);
    let connected = Connected {
        machine: link.machine.id.clone(),
        owner: link.machine.owner.clone(),
    };
    let mut attached = shared.links.open(connected, hello.capabilities);
    let mut pings = Pings::new(shared.sockets.ping_every);
    loop {
        tokio::select! {
            unpaired = link.unpaired.recv() => match unpaired {
                Ok(id) if id != link.machine.id => {}
                // A notice missed is checked against the database.
                Ok(_) | Err(RecvError::Lagged(_)) => link.paired(shared).await?,
                Err(RecvError::Closed) => return Err(Closed),
            },
            () = pings.due() => pings.ping(socket, link.paired(shared)).await?,
            Some(relayed) = attached.incoming.recv() => {
                reply(socket, shared, relayed.id, relayed.message).await?;
            }
            frame = socket.recv() => {
                let frame = frame.ok_or(Closed)?;
                pings.heard(&frame)?;
                if let Ok(Frame::Text(text)) = &frame
                    && let Ok(envelope) = Envelope::from_json(text.as_bytes())
                {
                    hand_off(shared, &link.machine.id, envelope);
                }
            }
        }
    }
}

/// Gives what `machine` sent to the feature it is for.
fn hand_off(shared: &Shared, machine: &MachineId, envelope: Envelope) {
    // The relay drops what isn't an answer it waits for; no other feature hears machines yet.
    let _claimed = shared.relay.answered(machine, envelope);
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as Wire;
    use weacon_protocol::{AccountId, Message, Welcome};

    use super::*;
    use crate::relay::{SESSIONS, SETTINGS};
    use crate::test_support::socket::{
        Socket, ends, frame, next, open, pongs, say, upgrade_status,
    };
    use crate::test_support::{Fixture, Paired, ask, pair_machine};

    const CONNECT_PATH: &str = "/api/machines/connect";

    // A paired machine's connection, welcomed: the header proving ada's sign-in, her session's
    // token, the machine, its token and the connection.
    async fn connected(address: SocketAddr, fixture: &Fixture) -> (Paired, Socket) {
        let paired = pair_machine(fixture, address).await;
        let mut client = open(address, CONNECT_PATH, &paired.token).await.unwrap();
        say(&mut client, &["machine.connect"]).await;
        let welcome = next(&mut client).await;
        assert!(
            matches!(&welcome, Message::Welcome(Welcome { capabilities, .. })
                if *capabilities == [CONNECT, SETTINGS, SESSIONS].map(Capability::from)),
            "{welcome:?}"
        );
        (paired, client)
    }

    #[tokio::test]
    async fn only_a_paired_machines_token_opens_a_connection() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, _client) = connected(server.address, &fixture).await;

        for token in ["", "nonsense", paired.session.as_str()] {
            let opened = open(server.address, CONNECT_PATH, token).await;
            assert_eq!(upgrade_status(opened), 401, "{token}");
        }
    }

    #[tokio::test]
    async fn a_core_must_offer_the_connection() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let mut client = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap();

        say(&mut client, &["chat.send"]).await;

        assert_eq!(
            next(&mut client).await,
            refusal(code!("capability.missing"))
        );
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn unpairing_a_machine_ends_its_connection_at_once() {
        let mut fixture = Fixture::new().await;
        // Pings far apart, so only the notice can end it in time.
        fixture.quiet();
        let server = fixture.run().await;
        let (paired, mut client) = connected(server.address, &fixture).await;
        let path = format!(
            "DELETE /api/machines/{}",
            paired.machine["id"].as_str().unwrap()
        );

        let (unpaired, _) = ask(server.address, &path, &paired.proof, "").await;
        let said = next(&mut client).await;
        let ended = ends(&mut client).await;
        let again = open(server.address, CONNECT_PATH, &paired.token).await;

        assert_eq!(unpaired, "http/1.1 204 no content");
        assert_eq!(said, refusal(code!("machine.unpaired")));
        assert!(ended);
        assert_eq!(upgrade_status(again), 401);
    }

    #[tokio::test]
    async fn a_connection_ends_at_its_next_ping_once_the_machine_is_gone() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, mut client) = connected(server.address, &fixture).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();

        // Unpaired without a notice, as by another server.
        let db = &fixture.shared.db;
        assert!(db.unpair(&account, &paired.machine_id()).await.unwrap());

        assert_eq!(next(&mut client).await, refusal(code!("machine.unpaired")));
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_paired_machines_connection_outlasts_its_pings() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, mut client) = connected(server.address, &fixture).await;
        // Reading answers each ping; the fixture pings every 100 ms.
        for _ in 0..4 {
            let got = frame(&mut client).await;
            assert!(matches!(got, Some(Ok(Wire::Ping(_)))), "{got:?}");
        }

        assert!(pongs(&mut client).await);
    }

    #[tokio::test]
    async fn a_machine_unpaired_before_its_hello_is_told_so() {
        let mut fixture = Fixture::new().await;
        // Pings far apart, so only the check after the hello can find it unpaired in time.
        fixture.quiet();
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let mut client = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap();
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();
        let db = &fixture.shared.db;
        assert!(db.unpair(&account, &paired.machine_id()).await.unwrap());

        say(&mut client, &["machine.connect"]).await;

        assert!(matches!(next(&mut client).await, Message::Welcome(_)));
        assert_eq!(next(&mut client).await, refusal(code!("machine.unpaired")));
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_machine_may_send_any_message_the_protocol_allows() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, mut client) = connected(server.address, &fixture).await;

        client
            .send(Wire::text("x".repeat(MAX_FRAME)))
            .await
            .unwrap();

        assert!(pongs(&mut client).await);
    }

    #[tokio::test]
    async fn another_machines_unpairing_leaves_a_connection_open() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();
        let server = fixture.run().await;
        let (_, mut client) = connected(server.address, &fixture).await;
        let other = MachineId::try_from("other".to_owned()).unwrap();

        fixture.shared.unpaired.send(other).unwrap();

        assert!(pongs(&mut client).await);
    }
}
