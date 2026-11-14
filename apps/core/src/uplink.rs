//! The daemon's connection to the server it is paired with: it dials out, proves the machine
//! with its token, and dials again with backoff whenever the connection drops.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hyper::StatusCode;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message as Wire;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AccountId, App, Capability, ChatCommand, Code, Envelope, ErrorBody, Hello, Message, Outcome,
    PROTOCOL, Receipt, code,
};

use crate::machine::{self, Paired};
use crate::server::{Failure, Server};

/// What the core must be offered by the server to keep its connection.
const CONNECT: Code = code!("machine.connect");
const PATH: &str = "/api/machines/connect";
/// How often a core that isn't paired looks for a pairing.
const LOOK_EVERY: Duration = Duration::from_secs(30);
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_mins(1);
const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// What a connection gives the rest of the daemon: its owner's commands.
pub(crate) type Commands = mpsc::Sender<ChatCommand>;

/// How long to wait before dialing again: doubling from a second to a minute, each wait
/// cut by up to a fifth at random so that cores don't all dial at once.
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    failures: u32,
}

impl Backoff {
    /// The wait before the next try, given a random number for its jitter.
    pub(crate) fn next(&mut self, random: u64) -> Duration {
        let doubled = FIRST_WAIT.saturating_mul(1 << self.failures.min(6));
        self.failures = self.failures.saturating_add(1);
        let wait = doubled.min(LONGEST_WAIT);
        let cut = wait / 5;
        let millis = u64::try_from(cut.as_millis()).unwrap_or(0);
        wait.saturating_sub(Duration::from_millis(
            random.checked_rem(millis.max(1)).unwrap_or(0),
        ))
    }

    pub(crate) fn reset(&mut self) {
        self.failures = 0;
    }
}

/// How a connection ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The server says the machine isn't paired.
    Unpaired,
    /// It broke, or never came up; worth dialing again. Whether it came up resets the backoff.
    Dropped { welcomed: bool },
}

/// When to dial again after a connection that didn't end in an unpairing.
#[derive(Debug, Default)]
struct Retry {
    backoff: Backoff,
    /// Whether the last try failed, so a run of failures is logged once.
    failing: bool,
}

impl Retry {
    fn after(&mut self, ended: Result<Ended, Fault>, log: &Logger, random: u64) -> Duration {
        match ended {
            Ok(Ended::Dropped { welcomed: true }) => {
                self.backoff.reset();
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
        self.backoff.next(random)
    }
}

/// Keeps the machine connected to its server for as long as the daemon runs.
pub(crate) struct Uplink {
    pub(crate) state_dir: PathBuf,
    pub(crate) app: App,
    pub(crate) capabilities: Vec<Capability>,
    pub(crate) log: Logger,
    pub(crate) commands: Commands,
    /// Random numbers, for the backoff's jitter.
    pub(crate) random: fn() -> u64,
}

impl Uplink {
    pub(crate) async fn run(self) -> ! {
        let mut retry = Retry::default();
        loop {
            let Some(paired) = self.paired().await else {
                sleep(LOOK_EVERY).await;
                continue;
            };
            let log = self
                .log
                .child(&[("machine_id", paired.machine.as_str().into())]);
            match self.connection(&paired, &log).await {
                Ok(Ended::Unpaired) => self.unpaired(&log).await,
                ended => sleep(retry.after(ended, &log, (self.random)())).await,
            }
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

    async fn unpaired(&self, log: &Logger) {
        log.warn("machine unpaired", &[]);
        let dir = self.state_dir.clone();
        let forgotten = tokio::task::spawn_blocking(move || machine::forget(&dir)).await;
        if let Ok(Err(fault)) = forgotten {
            log.warn("uplink failed", &[("err", Field::error(&fault))]);
        }
    }

    async fn connection(&self, paired: &Paired, log: &Logger) -> Result<Ended, Fault> {
        let server = Server::new(&paired.server, paired.pinned.as_deref().map(str::as_bytes))?;
        let mut socket = match server.socket(PATH, &paired.token).await {
            Ok(socket) => socket,
            Err(Failure::Refused(StatusCode::UNAUTHORIZED)) => return Ok(Ended::Unpaired),
            Err(Failure::Refused(_)) => return Ok(Ended::Dropped { welcomed: false }),
            Err(Failure::Unreachable(fault)) => return Err(fault),
        };
        if !self.greeted(&mut socket).await {
            return Ok(Ended::Dropped { welcomed: false });
        }
        log.info("uplink connected", &[]);
        while let Some(Ok(frame)) = socket.next().await {
            let Wire::Text(text) = frame else { continue };
            let Ok(envelope) = Envelope::from_json(text.as_bytes()) else {
                continue;
            };
            match self.answer(envelope, &paired.owner).await {
                Step::Unpaired => return Ok(Ended::Unpaired),
                Step::Reply(reply) => send(&mut socket, &reply).await,
                Step::Nothing => {}
            }
        }
        Ok(Ended::Dropped { welcomed: true })
    }

    /// Says hello, and whether the server welcomed it with the connection.
    async fn greeted<S>(&self, socket: &mut S) -> bool
    where
        S: SinkExt<Wire>
            + StreamExt<Item = Result<Wire, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let hello = Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol: PROTOCOL,
                app: self.app.clone(),
                capabilities: self.capabilities.clone(),
            }),
        };
        send(socket, &hello).await;
        let welcome = timeout(HELLO_WITHIN, socket.next()).await;
        let Ok(Some(Ok(Wire::Text(text)))) = welcome else {
            return false;
        };
        matches!(
            Envelope::from_json(text.as_bytes()),
            Ok(Envelope { message: Message::Welcome(welcome), .. })
                if welcome.capabilities.contains(&Capability::from(CONNECT))
        )
    }

    /// What to do with a message from the server. Only the machine's owner may ask it to act.
    async fn answer(&self, envelope: Envelope, owner: &AccountId) -> Step {
        if let Message::Error(ErrorBody { code, .. }) = &envelope.message
            && *code == code!("machine.unpaired")
        {
            return Step::Unpaired;
        }
        let Message::ChatCommand(command) = envelope.message else {
            return Step::Nothing;
        };
        let request = command.request.clone();
        let refused = |code| {
            let receipt = Receipt {
                request,
                outcome: Outcome::Failed { code },
            };
            Step::Reply(Envelope {
                id: envelope.id,
                message: Message::Receipt(receipt),
            })
        };
        if command.by.as_ref().is_none_or(|by| by.account != *owner) {
            return refused(code!("chat.not_owner"));
        }
        if self.commands.send(command).await.is_err() {
            return refused(code!("chat.unavailable"));
        }
        Step::Nothing
    }
}

enum Step {
    Unpaired,
    Reply(Envelope),
    Nothing,
}

/// Sends `envelope`; a connection that broke shows up on its next read.
async fn send<S: SinkExt<Wire> + Unpin>(socket: &mut S, envelope: &Envelope) {
    let Ok(json) = envelope.to_json() else { return };
    let Ok(text) = String::from_utf8(json) else {
        return;
    };
    drop(socket.send(Wire::text(text)).await);
}

/// The jitter's random numbers, from the operating system.
pub(crate) fn random() -> u64 {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_or(0, |()| u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use tokio_tungstenite::WebSocketStream;
    use weacon_protocol::{
        Asker, ChatAction, ChatId, MachineId, MemberId, RequestId, Welcome, WorkspaceId,
    };
    use weacon_testing::TempDir;

    use super::*;
    use crate::machine::Key;
    use crate::test_support::tls::{Tls, answer, read_request};
    use crate::test_support::{VERSION, messages, recording_logger};

    type Peer = WebSocketStream<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>;

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

    fn uplink(dir: &TempDir, commands: Commands) -> Uplink {
        Uplink {
            state_dir: dir.to_path_buf(),
            app: weacon_appkit::build(&VERSION),
            capabilities: vec![Capability::from(CONNECT)],
            log: recording_logger().0,
            commands,
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
        peer.send(text(&Envelope {
            id: 1,
            message: Message::Welcome(welcome),
        }))
        .await
        .unwrap();
        peer
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

    #[test]
    fn the_wait_doubles_to_a_minute_less_a_random_fifth_and_resets() {
        let mut backoff = Backoff::default();
        let waits: Vec<_> = (0..8).map(|_| backoff.next(0).as_secs()).collect();
        let jittered = backoff.next(11_999);
        backoff.reset();

        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(jittered, Duration::from_millis(48_001));
        assert_eq!(
            backoff.next(u64::MAX),
            Duration::from_millis(1000 - u64::MAX % 200)
        );
    }

    #[tokio::test]
    async fn only_the_owners_commands_reach_the_daemon_and_an_unpairing_ends_it() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let (commands, mut taken) = mpsc::channel(4);
        let link = uplink(&dir, commands);
        let pairing = paired(&tls);
        let stranger = AccountId::try_from("a-other".to_owned()).unwrap();
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT]).await;
            peer.send(text(&command("r-owner", Some(&owner()))))
                .await
                .unwrap();
            peer.send(text(&command("r-stranger", Some(&stranger))))
                .await
                .unwrap();
            peer.send(text(&command("r-nobody", None))).await.unwrap();
            let refused = [heard(&mut peer).await, heard(&mut peer).await];
            let unpaired = ErrorBody {
                code: code!("machine.unpaired"),
                retryable: false,
            };
            peer.send(text(&Envelope {
                id: 0,
                message: Message::Error(unpaired),
            }))
            .await
            .unwrap();
            refused
        };

        let (ended, refused) = tokio::join!(link.connection(&pairing, &link.log), serve);

        assert_eq!(ended.unwrap(), Ended::Unpaired);
        assert_eq!(taken.recv().await.unwrap().request.as_str(), "r-owner");
        taken.try_recv().unwrap_err();
        assert_eq!(refusal(&refused[0]), Some(("r-stranger", "chat.not_owner")));
        assert_eq!(refusal(&refused[1]), Some(("r-nobody", "chat.not_owner")));
    }

    #[tokio::test]
    async fn an_owners_command_nothing_takes_is_refused_as_unavailable() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let (commands, taken) = mpsc::channel(4);
        drop(taken);
        let link = uplink(&dir, commands);
        let pairing = paired(&tls);
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT]).await;
            peer.send(text(&command("r-owner", Some(&owner()))))
                .await
                .unwrap();
            let refused = heard(&mut peer).await;
            drop(peer);
            refused
        };

        let (ended, refused) = tokio::join!(link.connection(&pairing, &link.log), serve);

        assert_eq!(ended.unwrap(), Ended::Dropped { welcomed: true });
        assert_eq!(refusal(&refused), Some(("r-owner", "chat.unavailable")));
    }

    #[tokio::test]
    async fn a_server_that_doesnt_offer_the_connection_or_refuses_the_token_is_left() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir, mpsc::channel(1).0);
        let pairing = paired(&tls);
        let refuse = async {
            let mut stream = tls.accept().await.unwrap();
            read_request(&mut stream).await;
            answer(&mut stream, "401 Unauthorized", "{}").await;
        };

        let (unoffered, _) =
            tokio::join!(link.connection(&pairing, &link.log), welcoming(&tls, &[]));
        let (refused, ()) = tokio::join!(link.connection(&pairing, &link.log), refuse);

        assert_eq!(unoffered.unwrap(), Ended::Dropped { welcomed: false });
        assert_eq!(refused.unwrap(), Ended::Unpaired);
    }

    #[test]
    fn a_run_of_failures_is_logged_once_and_a_lost_connection_starts_the_wait_again() {
        let (log, records) = recording_logger();
        let mut retry = Retry::default();
        let failed = || {
            Err(Fault::new(
                code!("server.unreachable"),
                "The server couldn't be reached",
            ))
        };

        let waits = [
            retry.after(failed(), &log, 0),
            retry.after(failed(), &log, 0),
            retry.after(Ok(Ended::Dropped { welcomed: false }), &log, 0),
            retry.after(Ok(Ended::Dropped { welcomed: true }), &log, 0),
            retry.after(failed(), &log, 0),
        ];

        let secs: Vec<_> = waits.iter().map(Duration::as_secs).collect();
        assert_eq!(secs, [1, 2, 4, 1, 2]);
        assert_eq!(
            messages(&records),
            ["uplink failed", "uplink lost", "uplink failed"]
        );
    }

    #[tokio::test]
    async fn an_unpaired_machine_forgets_its_pairing() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        machine::write(&dir, &paired(&tls)).unwrap();
        let link = uplink(&dir, mpsc::channel(1).0);

        let found = link.paired().await;
        link.unpaired(&link.log).await;

        assert_eq!(found, Some(paired(&tls)));
        assert_eq!(link.paired().await, None);
    }

    #[tokio::test]
    async fn a_server_that_cant_be_reached_is_an_error() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir, mpsc::channel(1).0);
        let mut gone = paired(&tls);
        gone.server = "https://localhost:1".to_owned();

        let failed = link.connection(&gone, &link.log).await;

        assert_eq!(failed.unwrap_err().code().as_str(), "server.unreachable");
    }
}
