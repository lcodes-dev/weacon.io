//! The daemon's connection to the server it is paired with: it dials out, proves the machine
//! with its token, and dials again with backoff whenever the connection drops.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, timeout};
use tokio_tungstenite::tungstenite::{Message as Wire, Utf8Bytes};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AccountId, App, Capability, ChatCommand, Code, Envelope, ErrorBody, Hello, Message, Outcome,
    PROTOCOL, Receipt, code,
};

use crate::machine::{self, Paired};
use crate::server::{Failure, Server, Socket};

/// What the core must be offered by the server to keep its connection.
const CONNECT: Code = code!("machine.connect");
const PATH: &str = "/api/machines/connect";
/// How often a core that isn't paired looks for a pairing.
const LOOK_EVERY: Duration = Duration::from_secs(30);
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_mins(1);
const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// How long a connection may stay quiet, and how long it must last to count as steady.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// The server pings every 30 seconds, so a connection this quiet is gone.
    pub(crate) quiet_within: Duration,
    /// A connection lost sooner backs off as if it never came up.
    pub(crate) steady_after: Duration,
}

pub(crate) const TIMING: Timing = Timing {
    quiet_within: Duration::from_secs(75),
    steady_after: Duration::from_secs(30),
};

/// Where the owner's chat commands go in the daemon.
pub(crate) type Commands = mpsc::Sender<ChatCommand>;

/// How a connection ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The server says the machine isn't paired.
    Unpaired,
    /// It was up and steady, and broke or went quiet.
    Lost,
    /// It never came up, the server refused it or didn't welcome it, or it broke soon after.
    Unwelcome,
}

/// When to dial again: a wait doubling from a second to a minute, each cut by up to a fifth
/// at random so that cores don't all dial at once, and back to a second once a connection was
/// up.
#[derive(Debug, Default)]
struct Retry {
    failures: u32,
    /// Whether the last try failed, so a run of failures is logged once.
    failing: bool,
}

impl Retry {
    fn after(&mut self, ended: Result<Ended, Fault>, log: &Logger, random: u128) -> Duration {
        match ended {
            Ok(Ended::Lost) => {
                self.failures = 0;
                self.failing = false;
                log.info("uplink lost", &[]);
            }
            Ok(_) => {}
            Err(fault) => {
                if !self.failing {
                    log.warn("uplink failed", &[("err", Field::error(&fault))]);
                }
                self.failing = true;
            }
        }
        let wait = FIRST_WAIT
            .saturating_mul(1 << self.failures.min(6))
            .min(LONGEST_WAIT);
        self.failures = self.failures.saturating_add(1);
        let fifth = (wait / 5).as_millis().max(1);
        let cut = u64::try_from(random % fifth).unwrap_or(0);
        wait.saturating_sub(Duration::from_millis(cut))
    }
}

/// Keeps the machine connected to its server for as long as the daemon runs.
pub(crate) struct Uplink {
    pub(crate) state_dir: PathBuf,
    pub(crate) app: App,
    pub(crate) capabilities: Vec<Capability>,
    pub(crate) log: Logger,
    pub(crate) commands: Commands,
    pub(crate) timing: Timing,
    /// Random numbers, for the backoff's jitter.
    pub(crate) random: fn() -> u128,
}

impl Uplink {
    pub(crate) async fn run(self) -> ! {
        let mut retry = Retry::default();
        loop {
            let wait = self.step(&mut retry).await;
            sleep(wait).await;
        }
    }

    /// Connects once if the machine is paired, and says how long to wait before the next try.
    async fn step(&self, retry: &mut Retry) -> Duration {
        let Some(paired) = self.paired().await else {
            return LOOK_EVERY;
        };
        let log = self
            .log
            .child(&[("machine_id", paired.machine.as_str().into())]);
        match self.connection(&paired, &log).await {
            Ok(Ended::Unpaired) => {
                self.unpaired(&paired, &log).await;
                LOOK_EVERY
            }
            ended => retry.after(ended, &log, (self.random)()),
        }
    }

    /// The pairing, read off the async runtime; one that can't be read is logged and skipped.
    async fn paired(&self) -> Option<Paired> {
        let dir = self.state_dir.clone();
        match tokio::task::spawn_blocking(move || machine::read(&dir)).await {
            Ok(Ok(paired)) => paired,
            Ok(Err(fault)) => {
                self.log
                    .warn("uplink failed", &[("err", Field::error(&fault))]);
                None
            }
            Err(_) => None,
        }
    }

    async fn unpaired(&self, paired: &Paired, log: &Logger) {
        log.warn("machine unpaired", &[]);
        let (dir, paired) = (self.state_dir.clone(), paired.clone());
        let forgotten = tokio::task::spawn_blocking(move || machine::forget(&dir, &paired)).await;
        if let Ok(Err(fault)) = forgotten {
            log.warn("uplink failed", &[("err", Field::error(&fault))]);
        }
    }

    async fn connection(&self, paired: &Paired, log: &Logger) -> Result<Ended, Fault> {
        let server = Server::new(&paired.server, paired.pinned.as_deref().map(str::as_bytes))?;
        let mut socket = match server.socket(PATH, &paired.token).await {
            Ok(socket) => socket,
            Err(Failure::Refused(401, Some(code))) if code == code!("machine.unpaired") => {
                return Ok(Ended::Unpaired);
            }
            Err(Failure::Refused(..)) => return Ok(Ended::Unwelcome),
            Err(Failure::Unreachable(fault)) => return Err(fault),
        };
        if !self.greeted(&mut socket).await {
            return Ok(Ended::Unwelcome);
        }
        log.info("uplink connected", &[]);
        let up = Instant::now();
        let quiet_within = self.timing.quiet_within;
        while let Ok(Some(Ok(frame))) = timeout(quiet_within, socket.next()).await {
            let Wire::Text(text) = frame else { continue };
            let Ok(envelope) = Envelope::from_json(text.as_bytes()) else {
                continue;
            };
            match self.answer(envelope, &paired.owner) {
                Step::Unpaired => return Ok(Ended::Unpaired),
                Step::Reply(reply) => send(&mut socket, &reply).await,
                Step::Nothing => {}
            }
        }
        let steady = up.elapsed() >= self.timing.steady_after;
        Ok(if steady {
            Ended::Lost
        } else {
            Ended::Unwelcome
        })
    }

    /// Says hello, and whether the server welcomed it with the connection.
    async fn greeted(&self, socket: &mut Socket) -> bool {
        let hello = Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol: PROTOCOL,
                app: self.app.clone(),
                capabilities: self.capabilities.clone(),
            }),
        };
        send(socket, &hello).await;
        let Ok(Some(text)) = timeout(HELLO_WITHIN, first_text(socket)).await else {
            return false;
        };
        matches!(
            Envelope::from_json(text.as_bytes()),
            Ok(Envelope { message: Message::Welcome(welcome), .. })
                if welcome.capabilities.contains(&Capability::from(CONNECT))
        )
    }
}

impl Uplink {
    /// What to do with a message from the server. Only the machine's owner may ask it to act,
    /// whatever the server allows.
    fn answer(&self, envelope: Envelope, owner: &AccountId) -> Step {
        if let Message::Error(ErrorBody { code, .. }) = &envelope.message
            && *code == code!("machine.unpaired")
        {
            return Step::Unpaired;
        }
        let Message::ChatCommand(command) = envelope.message else {
            return Step::Nothing;
        };
        let request = command.request.clone();
        let from_owner = command.by.as_ref().is_some_and(|by| by.account == *owner);
        let code = if !from_owner {
            code!("chat.not_owner")
        } else if self.commands.try_send(command).is_err() {
            code!("chat.unavailable")
        } else {
            return Step::Nothing;
        };
        Step::Reply(Envelope {
            id: envelope.id,
            message: Message::Receipt(Receipt {
                request,
                outcome: Outcome::Failed { code },
            }),
        })
    }
}

enum Step {
    Unpaired,
    Reply(Envelope),
    Nothing,
}

/// The next text frame, past any pings.
async fn first_text(socket: &mut Socket) -> Option<Utf8Bytes> {
    loop {
        match socket.next().await? {
            Ok(Wire::Text(text)) => return Some(text),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

/// Sends `envelope`; a connection that broke shows up on its next read.
async fn send(socket: &mut Socket, envelope: &Envelope) {
    let Ok(json) = envelope.to_json() else { return };
    let Ok(text) = String::from_utf8(json) else {
        return;
    };
    drop(socket.send(Wire::text(text)).await);
}

#[cfg(test)]
mod tests {
    use tokio_rustls::server::TlsStream;
    use tokio_tungstenite::WebSocketStream;
    use weacon_protocol::{
        Asker, ChatAction, ChatCommand, ChatId, MachineId, MemberId, RequestId, Welcome,
        WorkspaceId,
    };
    use weacon_testing::TempDir;

    use super::*;
    use crate::machine::Key;
    use crate::test_support::tls::{Tls, answer, nowhere, read_request};
    use crate::test_support::{VERSION, messages, recording_logger};

    type Peer = WebSocketStream<TlsStream<tokio::net::TcpStream>>;

    fn owner() -> AccountId {
        AccountId::try_from("a-owner".to_owned()).unwrap()
    }

    fn paired(tls: &Tls) -> Paired {
        Paired {
            server: tls.url(),
            pinned: Some(tls.pem.clone()),
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: owner(),
            token: "t0ken".to_owned(),
            key: Key::from_random([7; 32]),
        }
    }

    fn uplink(dir: &TempDir) -> Uplink {
        with_commands(dir, mpsc::channel(1).0)
    }

    fn with_commands(dir: &TempDir, commands: Commands) -> Uplink {
        Uplink {
            state_dir: dir.to_path_buf(),
            app: weacon_appkit::build(&VERSION),
            capabilities: vec![Capability::from(CONNECT)],
            log: recording_logger().0,
            commands,
            timing: Timing {
                quiet_within: Duration::from_secs(10),
                steady_after: Duration::ZERO,
            },
            random: || 0,
        }
    }

    fn text(envelope: &Envelope) -> Wire {
        Wire::text(String::from_utf8(envelope.to_json().unwrap()).unwrap())
    }

    async fn heard(peer: &mut Peer) -> Message {
        loop {
            if let Some(Ok(Wire::Text(text))) = peer.next().await {
                return Envelope::from_json(text.as_bytes()).unwrap().message;
            }
        }
    }

    // A server that takes the socket, checks the hello and welcomes it with `offers`.
    async fn welcoming(tls: &Tls, offers: &[Code]) -> Peer {
        let stream = tls.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        let Message::Hello(hello) = heard(&mut peer).await else {
            panic!("no hello");
        };
        assert_eq!(hello.protocol, PROTOCOL);
        assert_eq!(hello.capabilities, [Capability::from(CONNECT)]);
        let welcome = Welcome {
            app: weacon_appkit::build(&VERSION),
            capabilities: offers.iter().cloned().map(Capability::from).collect(),
        };
        let envelope = Envelope {
            id: 1,
            message: Message::Welcome(welcome),
        };
        peer.send(text(&envelope)).await.unwrap();
        peer
    }

    // Sends `envelope` and gives what the core says back.
    async fn asked(peer: &mut Peer, envelope: &Envelope) -> Message {
        peer.send(text(envelope)).await.unwrap();
        heard(peer).await
    }

    // A server that refuses the socket's handshake with `status` and `code`.
    async fn refusing(tls: &Tls, status: &str, code: Code) {
        let mut stream = tls.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = ErrorBody {
            code,
            retryable: false,
        };
        answer(&mut stream, status, &serde_json::to_string(&body).unwrap()).await;
    }

    fn command(request: &str, by: Option<&AccountId>) -> Envelope {
        Envelope {
            id: 9,
            message: Message::ChatCommand(ChatCommand {
                request: RequestId::try_from(request.to_owned()).unwrap(),
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                action: ChatAction::Interrupt,
                by: by.map(|account| Asker {
                    account: account.clone(),
                    member: MemberId::try_from("m-1".to_owned()).unwrap(),
                }),
            }),
        }
    }

    fn refusal(message: &Message) -> Option<(&str, &str)> {
        let Message::Receipt(Receipt {
            request,
            outcome: Outcome::Failed { code },
        }) = message
        else {
            return None;
        };
        Some((request.as_str(), code.as_str()))
    }

    fn unreachable() -> Result<Ended, Fault> {
        Err(Fault::new(
            code!("server.unreachable"),
            "The server couldn't be reached",
        ))
    }

    #[test]
    fn the_wait_doubles_to_a_minute_less_a_random_fifth_and_resets_once_up() {
        let (log, records) = recording_logger();
        let mut retry = Retry::default();
        let mut waits: Vec<_> = (0..4)
            .map(|_| retry.after(unreachable(), &log, 0).as_secs())
            .collect();
        waits.extend((0..4).map(|_| retry.after(Ok(Ended::Unwelcome), &log, 0).as_secs()));
        let jittered = retry.after(unreachable(), &log, 11_999);
        let after_up = retry.after(Ok(Ended::Lost), &log, 0);
        let failing_again = retry.after(unreachable(), &log, 1_999);

        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(jittered, Duration::from_millis(48_001));
        assert_eq!(after_up, Duration::from_secs(1));
        assert_eq!(failing_again, Duration::from_millis(1_601));
        assert_eq!(
            messages(&records),
            ["uplink failed", "uplink lost", "uplink failed"]
        );
    }

    #[tokio::test]
    async fn only_the_owners_commands_reach_the_daemon_and_an_unpairing_ends_it() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let (commands, mut taken) = mpsc::channel(4);
        let link = with_commands(&dir, commands);
        let pairing = paired(&tls);
        let stranger = AccountId::try_from("a-other".to_owned()).unwrap();
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT]).await;
            peer.send(text(&command("r-owner", Some(&owner()))))
                .await
                .unwrap();
            let heard_back = [
                asked(&mut peer, &command("r-stranger", Some(&stranger))).await,
                asked(&mut peer, &command("r-nobody", None)).await,
            ];
            let unpaired = ErrorBody {
                code: code!("machine.unpaired"),
                retryable: false,
            };
            let envelope = Envelope {
                id: 0,
                message: Message::Error(unpaired),
            };
            peer.send(text(&envelope)).await.unwrap();
            heard_back
        };

        let (ended, heard_back) = tokio::join!(link.connection(&pairing, &link.log), serve);

        assert_eq!(ended.unwrap(), Ended::Unpaired);
        let refusals: Vec<_> = heard_back.iter().map(refusal).collect();
        assert_eq!(
            refusals,
            [
                Some(("r-stranger", "chat.not_owner")),
                Some(("r-nobody", "chat.not_owner")),
            ]
        );
        assert_eq!(taken.recv().await.unwrap().request.as_str(), "r-owner");
        taken.try_recv().unwrap_err();
    }

    #[tokio::test]
    async fn an_owners_command_nothing_takes_is_refused_as_unavailable() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = with_commands(&dir, mpsc::channel(1).0);
        let pairing = paired(&tls);
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT]).await;
            asked(&mut peer, &command("r-owner", Some(&owner()))).await
        };

        let (ended, refused) = tokio::join!(link.connection(&pairing, &link.log), serve);

        assert_eq!(ended.unwrap(), Ended::Lost);
        assert_eq!(refusal(&refused), Some(("r-owner", "chat.unavailable")));
    }

    #[tokio::test]
    async fn a_connection_that_breaks_is_lost() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let pairing = paired(&tls);

        let (ended, ()) = tokio::join!(link.connection(&pairing, &link.log), async {
            drop(welcoming(&tls, &[CONNECT]).await);
        });

        assert_eq!(ended.unwrap(), Ended::Lost);
    }

    #[tokio::test]
    async fn a_server_that_doesnt_offer_the_connection_or_refuses_the_token_is_left() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let pairing = paired(&tls);

        let (unoffered, _) =
            tokio::join!(link.connection(&pairing, &link.log), welcoming(&tls, &[]));
        let (unauthorized, ()) = tokio::join!(
            link.connection(&pairing, &link.log),
            refusing(&tls, "401 Unauthorized", code!("machine.unpaired"))
        );
        let (busy, ()) = tokio::join!(
            link.connection(&pairing, &link.log),
            refusing(&tls, "401 Unauthorized", code!("proxy.sign_in"))
        );

        assert_eq!(unoffered.unwrap(), Ended::Unwelcome);
        assert_eq!(unauthorized.unwrap(), Ended::Unpaired);
        assert_eq!(busy.unwrap(), Ended::Unwelcome);
    }

    #[tokio::test]
    async fn a_connection_lost_at_once_or_gone_quiet_is_told_apart() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let mut link = uplink(&dir);
        link.timing = Timing {
            quiet_within: Duration::from_millis(100),
            steady_after: Duration::from_hours(1),
        };
        let pairing = paired(&tls);

        let (at_once, ()) = tokio::join!(link.connection(&pairing, &link.log), async {
            drop(welcoming(&tls, &[CONNECT]).await);
        });
        link.timing.steady_after = Duration::ZERO;
        let offers = [CONNECT];
        let (quiet, _peer) = tokio::join!(
            link.connection(&pairing, &link.log),
            welcoming(&tls, &offers)
        );

        assert_eq!(at_once.unwrap(), Ended::Unwelcome);
        assert_eq!(quiet.unwrap(), Ended::Lost);
    }

    #[tokio::test]
    async fn a_ping_before_the_welcome_is_skipped() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let pairing = paired(&tls);
        let serve = async {
            let stream = tls.accept().await.unwrap();
            let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
            peer.send(Wire::Ping(Vec::new().into())).await.unwrap();
            heard(&mut peer).await;
            let welcome = Welcome {
                app: weacon_appkit::build(&VERSION),
                capabilities: vec![Capability::from(CONNECT)],
            };
            let envelope = Envelope {
                id: 1,
                message: Message::Welcome(welcome),
            };
            peer.send(text(&envelope)).await.unwrap();
        };

        let (ended, ()) = tokio::join!(link.connection(&pairing, &link.log), serve);

        assert_eq!(ended.unwrap(), Ended::Lost);
    }

    #[tokio::test]
    async fn after_an_unpairing_or_without_a_pairing_the_core_waits_to_look_again() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let mut retry = Retry::default();
        machine::write(&dir, &paired(&tls)).unwrap();

        let (unpaired, ()) = tokio::join!(
            link.step(&mut retry),
            refusing(&tls, "401 Unauthorized", code!("machine.unpaired"))
        );
        let alone = link.step(&mut retry).await;

        assert_eq!(unpaired, LOOK_EVERY);
        assert_eq!(alone, LOOK_EVERY);
        assert_eq!(machine::read(&dir).unwrap(), None);
    }

    #[tokio::test]
    async fn an_unpaired_machine_forgets_its_pairing() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        machine::write(&dir, &paired(&tls)).unwrap();
        let link = uplink(&dir);

        let found = link.paired().await;
        link.unpaired(&paired(&tls), &link.log).await;

        assert_eq!(found, Some(paired(&tls)));
        assert_eq!(link.paired().await, None);
    }

    #[tokio::test]
    async fn a_server_that_cant_be_reached_is_an_error() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let mut gone = paired(&tls);
        gone.server = nowhere();

        let failed = link.connection(&gone, &link.log).await;

        assert_eq!(failed.unwrap_err().code().as_str(), "server.unreachable");
    }
}
