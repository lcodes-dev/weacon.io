//! A paired machine's connection, which its core dials out to and keeps open.

use std::convert::Infallible;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as Frame, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::timeout;
use weacon_observability::{Field, Logger};
use weacon_protocol::{
    App, Capability, Code, Envelope, Hello, MAX_FRAME, MachineId, MachineName, Message, code,
};

use crate::auth::Machine;
use crate::chats;
use crate::links::Connected;
use crate::proposals::{PROPOSALS, machines as proposals};
use crate::relay::{DIFF, SESSIONS, SETTINGS, SYMBOLS, WORKTREE};
use crate::shared::Shared;
use crate::sharing;
use crate::socket::{Closed, Pings, hear, refusal, reply, send};
use crate::terminals;

/// What a core must offer, and the server offers, to keep a machine's connection.
const CONNECT: Code = code!("machine.connect");

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/machines/connect", get(connect))
}

/// `GET /api/machines/connect`: a paired machine's WebSocket, open until the machine is
/// unpaired, its core goes, or the server stops.
pub(crate) async fn connect(
    State(shared): State<Shared>,
    machine: Machine,
    upgrade: WebSocketUpgrade,
) -> Response {
    let unpaired = shared.unpaired.subscribe();
    let log = shared
        .log
        .child(&[("machine_id", machine.id.as_str().into())]);
    let link = Link {
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
    terminals::offline(&shared, &link.machine.id);
    // A core that is gone just doesn't hear it.
    if link.gone {
        drop(reply(&mut socket, &shared, 0, refusal(code!("machine.unpaired"))).await);
    }
    drop(send(&mut socket, shared.sockets.ping_every, Frame::Close(None)).await);
    if link.connected {
        link.log.info("machine disconnected", &[]);
    }
}

/// What the server offers every core, and needs of it for all its features to reach it.
pub(crate) fn offers() -> Vec<Capability> {
    [
        CONNECT,
        SETTINGS,
        DIFF,
        WORKTREE,
        SESSIONS,
        SYMBOLS,
        PROPOSALS,
        chats::SERVE,
        chats::LIVE,
    ]
    .map(Capability::new)
    .into()
}

/// The most capabilities of a core's hello that the machines list keeps.
const MOST_KEPT_OFFERS: usize = 64;

/// What of `hello` the machines list keeps: its build if every part reads as a machine name, and
/// all its capabilities or none.
fn kept(hello: &Hello) -> (Option<&App>, Option<&[Capability]>) {
    let build = &hello.app;
    let plain = [&build.name, &build.version, &build.commit]
        .into_iter()
        .all(|part| MachineName::try_from(part.clone()).is_ok());
    let offers = &hello.capabilities;
    let whole = offers.len() <= MOST_KEPT_OFFERS;
    (plain.then_some(build), whole.then_some(offers.as_slice()))
}

async fn run(
    socket: &mut WebSocket,
    shared: &Shared,
    link: &mut Link,
) -> Result<Infallible, Closed> {
    let offer = Capability::new(CONNECT);
    let mut offered = offers();
    // A core uploads its chats' events without offering it back.
    offered.push(Capability::new(chats::upload::UPLOAD));
    let heard = hear(socket, shared, offered, &offer);
    let (hello, welcome) = timeout(shared.sockets.hello_within, heard)
        .await
        .map_err(|_| Closed)??;
    // It may have been unpaired since it was let in, before it could hear of it.
    link.paired(shared).await?;
    link.connected = true;
    link.log.info("machine connected", &[]);
    let (build, offered) = kept(&hello);
    if let Err(fault) = shared.db.saw_core(&link.machine.id, build, offered).await {
        // The machines list shows it as it last was; the connection goes on.
        link.log
            .warn("core build not kept", &[("err", Field::error(&fault))]);
    }
    let connected = Connected {
        machine: link.machine.id.clone(),
        owner: link.machine.owner.clone(),
    };
    let mut attached = shared.links.open(connected, hello.capabilities);
    // Welcomed only once attached, so whatever a welcomed core is asked reaches it.
    reply(socket, shared, welcome.id, welcome.answer).await?;
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
            relayed = attached.incoming.recv() => {
                let relayed = relayed.ok_or(Closed)?;
                reply(socket, shared, relayed.id, relayed.message).await?;
            }
            frame = socket.recv() => {
                let frame = frame.ok_or(Closed)?;
                pings.heard(&frame)?;
                if let Ok(Frame::Text(text)) = &frame
                    && let Ok(envelope) = Envelope::from_json(text.as_bytes())
                {
                    let id = envelope.id;
                    if let Some(answer) = hand_off(shared, &link.machine, envelope).await? {
                        reply(socket, shared, id, answer).await?;
                    }
                }
            }
        }
    }
}

/// Gives what `machine` sent to the feature it is for, in the order it came, and gives what to
/// answer, if anything.
async fn hand_off(
    shared: &Shared,
    machine: &Machine,
    envelope: Envelope,
) -> Result<Option<Message>, Closed> {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "the relay takes every other message a machine sends"
    )]
    match envelope.message {
        Message::Chat(update) => chats::upload::store(shared, &machine.id, update).await?,
        Message::Terminal(update) => terminals::delivered(shared, &machine.id, update),
        Message::ChatLive(live) => chats::live::pass(shared, &machine.id, live).await,
        Message::MachineShares(told) => sharing::keep(shared, machine, told).await?,
        Message::ProposalCommand(command) => {
            let receipt = proposals::command(shared, machine, command).await;
            return Ok(Some(Message::Receipt(receipt)));
        }
        Message::ProposalQuery(query) => {
            let answer = proposals::query(shared, machine, query).await;
            return Ok(Some(Message::ProposalAnswer(answer)));
        }
        // The relay passes over what answers none of its asks; an error it doesn't claim is
        // for no other feature yet.
        message => {
            let envelope = Envelope {
                id: envelope.id,
                message,
            };
            let _claimed = shared.relay.answered(&machine.id, envelope);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use futures_util::SinkExt;
    use tokio::time::timeout;
    use tokio_tungstenite::tungstenite::Message as Wire;
    use weacon_protocol::{AccountId, ChatEntry, ChatUpdate, Message, PROTOCOL, Welcome};

    use super::*;
    use crate::db::test_support::{NOW, id, make, said};
    use crate::test_support::socket::{
        Socket, ends, frame, next, next_envelope, open, pongs, say, upgrade_status,
    };
    use crate::test_support::{Fixture, Paired, ask, pair_machine, run};

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
                if *capabilities == [
                    CONNECT,
                    SETTINGS,
                    DIFF,
                    WORKTREE,
                    SESSIONS,
                    SYMBOLS,
                    PROPOSALS,
                    code!("chat.serve"),
                    code!("chat.live"),
                    code!("chat.upload"),
                ].map(Capability::new)),
            "{welcome:?}"
        );
        (paired, client)
    }

    #[test]
    fn only_a_plain_build_and_every_offer_up_to_the_most_kept_are_kept() {
        let hello = |version: &str, offered: Vec<String>| Hello {
            protocol: PROTOCOL,
            app: App {
                name: "weacon-core".to_owned(),
                version: version.to_owned(),
                commit: "abc".to_owned(),
            },
            capabilities: serde_json::from_value(serde_json::json!(offered)).unwrap(),
        };
        let named = |count: usize| (0..count).map(|n| format!("future.thing{n}")).collect();
        let most = hello("1.2.3", named(MOST_KEPT_OFFERS));
        let (build, offers) = kept(&most);
        assert_eq!(build, Some(&most.app));
        assert_eq!(offers, Some(most.capabilities.as_slice()));
        let more = hello("1.2.3", named(MOST_KEPT_OFFERS + 1));
        assert_eq!(kept(&more), (Some(&more.app), None));
        for version in ["1.2.3\u{202e}", "1.2.3\n", &"9".repeat(65), ""] {
            assert_eq!(
                kept(&hello(version, Vec::new())),
                (None, Some(&[][..])),
                "{version:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_machines_list_shows_a_cores_build_and_what_it_doesnt_offer() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        // Welcomed once its build is kept.
        let (paired, _client) = connected(server.address, &fixture).await;

        let (_, list) = ask(server.address, "GET /api/machines", &paired.proof, "").await;

        let machine = &list["machines"][0];
        let build = weacon_appkit::build(&crate::test_support::VERSION);
        assert_eq!(machine["core"]["version"], build.version.as_str());
        assert_eq!(machine["core"]["commit"], build.commit.as_str());
        let missing = [
            "machine.settings",
            "diff.view",
            "worktree.manage",
            "sessions.read",
            "symbols.search",
            "workspace.proposals",
            "chat.serve",
            "chat.live",
        ];
        assert_eq!(machine["missing"], serde_json::json!(missing));
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
        let fixture = Fixture::new().await;
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
        let mut fixture = Fixture::new().await;
        fixture.pinging();
        let server = fixture.run().await;
        let (paired, mut client) = connected(server.address, &fixture).await;
        // Unpaired without a notice, as by another server, while the client reads on, so it
        // answers every ping until then.
        let unpair = async {
            let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
            let account = session["account"].as_str().unwrap().to_owned();
            let account = AccountId::try_from(account).unwrap();
            fixture
                .shared
                .db
                .unpair(&account, &paired.machine_id())
                .await
        };

        let (unpaired, said) = tokio::join!(unpair, next(&mut client));

        assert!(unpaired.unwrap());
        assert_eq!(said, refusal(code!("machine.unpaired")));
        assert!(ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_welcomed_core_is_reachable_at_once() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let mut core = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap();
        say(&mut core, &["machine.connect", "machine.settings"]).await;
        assert!(matches!(next(&mut core).await, Message::Welcome(_)));

        let asked = Envelope {
            id: 9,
            message: refusal(code!("machine.test")),
        };
        let settings = Capability::new(SETTINGS);
        let sent = (fixture.shared.links).send(&paired.machine_id(), &settings, asked.clone());

        assert_eq!(sent, Ok(()));
        assert_eq!(next_envelope(&mut core).await, asked);
    }

    #[tokio::test]
    async fn a_machine_that_connects_again_ends_its_older_connection() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, mut older) = connected(server.address, &fixture).await;
        let mut newer = open(server.address, CONNECT_PATH, &paired.token)
            .await
            .unwrap();
        say(&mut newer, &["machine.connect", "machine.settings"]).await;
        assert!(matches!(next(&mut newer).await, Message::Welcome(_)));

        let ended = ends(&mut older).await;
        let asked = Envelope {
            id: 9,
            message: refusal(code!("machine.test")),
        };
        let settings = Capability::new(SETTINGS);
        let sent = (fixture.shared.links).send(&paired.machine_id(), &settings, asked.clone());

        assert!(ended);
        assert_eq!(sent, Ok(()));
        assert_eq!(next_envelope(&mut newer).await, asked);
    }

    #[tokio::test]
    async fn a_paired_machines_connection_outlasts_its_pings() {
        let mut fixture = Fixture::new().await;
        fixture.pinging();
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
        let fixture = Fixture::new().await;
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

        // It isn't welcomed, so nothing it could be asked is relayed to it.
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
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, mut client) = connected(server.address, &fixture).await;
        let other = MachineId::try_from("other".to_owned()).unwrap();

        fixture.shared.unpaired.send(other).unwrap();

        assert!(pongs(&mut client).await);
    }

    #[tokio::test]
    async fn a_machines_chat_events_are_stored_in_the_order_they_come() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (paired, mut client) = connected(server.address, &fixture).await;
        let machine: MachineId = serde_json::from_value(paired.machine["id"].clone()).unwrap();
        let workspace = fixture.shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&machine).await;
        let mut stored = fixture.shared.chats.notices();

        for seq in [1, 2] {
            let update = ChatUpdate {
                workspace: id("w1"),
                chat: id("c-1"),
                entry: ChatEntry {
                    seq,
                    event: said("more"),
                },
            };
            let message = Message::Chat(update);
            crate::test_support::socket::send(&mut client, Envelope { id: 0, message }).await;
        }
        for _ in [1, 2] {
            timeout(Duration::from_secs(10), stored.recv())
                .await
                .unwrap()
                .unwrap();
        }

        let chat = workspace.chat(&id("c-1")).await.unwrap().unwrap();
        let core_seq = crate::db::test_support::core_seq(&workspace, "c-1").await;
        assert_eq!((chat.seq, core_seq), (2, 2));
    }
}
