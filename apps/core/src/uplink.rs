//! The daemon's connection to the server it is paired with: it dials out, proves the machine
//! with its token, and dials again with backoff whenever the connection drops.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until, timeout, timeout_at};
use tokio_tungstenite::tungstenite::{Message as Wire, Utf8Bytes};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AccountId, App, Capability, ChatCommand, Code, Envelope, ErrorBody, Hello, Message, Outcome,
    PROTOCOL, Receipt, Welcome, code,
};

use self::asking::{Asking, asking};
use self::chats::{ChatLink, LIVE, UPLOAD};
use self::retry::Retry;
use crate::chats::Chats;
use crate::machine::{self, Paired};
use crate::proposals;
use crate::server::{Failure, Server, Socket};

/// What the core must be offered by the server to keep its connection.
const CONNECT: Code = code!("machine.connect");
/// What the server offers that takes the core's proposals.
pub(crate) const PROPOSALS: Code = code!("workspace.proposals");
const PATH: &str = "/api/machines/connect";
/// How often a core that isn't paired looks for a pairing.
const LOOK_EVERY: Duration = Duration::from_secs(30);
const HELLO_WITHIN: Duration = Duration::from_secs(10);
/// How many messages wait to cross between the daemon and the uplink.
pub(crate) const QUEUE: usize = 64;

mod asking;
mod chats;
mod retry;

/// How long a connection may stay quiet, and how long it must last to count as steady.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// The server pings every 30 seconds, so a connection this quiet is gone.
    pub(crate) quiet_within: Duration,
    /// A connection lost sooner backs off as if it never came up.
    pub(crate) steady_after: Duration,
    /// How often an open connection checks that its pairing is still the one kept.
    pub(crate) look_every: Duration,
}

pub(crate) const TIMING: Timing = Timing {
    quiet_within: Duration::from_secs(75),
    steady_after: Duration::from_secs(30),
    look_every: LOOK_EVERY,
};

/// Where the messages from the server go in the daemon when neither the uplink nor the chats
/// take them.
pub(crate) type ToDaemon = mpsc::Sender<Envelope>;
/// What the daemon sends the server, while a connection is up.
pub(crate) type FromDaemon = mpsc::Receiver<Envelope>;

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

/// Keeps the machine connected to its server for as long as the daemon runs.
pub(crate) struct Uplink {
    pub(crate) state_dir: PathBuf,
    pub(crate) app: App,
    /// What it offers the server, worked out again at each hello from its state folder.
    pub(crate) capabilities: fn(&Path) -> Vec<Capability>,
    pub(crate) log: Logger,
    pub(crate) to_daemon: ToDaemon,
    pub(crate) chats: Chats,
    pub(crate) timing: Timing,
    /// Random numbers, for the backoff's jitter.
    pub(crate) random: fn() -> u128,
}

impl Uplink {
    pub(crate) async fn run(self, mut from_daemon: FromDaemon) -> ! {
        let mut retry = Retry::default();
        loop {
            let wait = self.step(&mut retry, &mut from_daemon).await;
            sleep(wait).await;
        }
    }

    /// Connects once if the machine is paired, and says how long to wait before the next try.
    async fn step(&self, retry: &mut Retry, from_daemon: &mut FromDaemon) -> Duration {
        let Some(paired) = self.paired().await else {
            return LOOK_EVERY;
        };
        let log = self
            .log
            .child(&[("machine_id", paired.machine.as_str().into())]);
        match self.connection(&paired, &log, from_daemon).await {
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

    async fn connection(
        &self,
        paired: &Paired,
        log: &Logger,
        from_daemon: &mut FromDaemon,
    ) -> Result<Ended, Fault> {
        let server = Server::new(&paired.server, paired.pinned.as_deref().map(str::as_bytes))?;
        let mut socket = match server.socket(PATH, &paired.token).await {
            Ok(socket) => socket,
            Err(Failure::Refused(401, Some(code))) if code == code!("machine.unpaired") => {
                return Ok(Ended::Unpaired);
            }
            Err(Failure::Refused(..)) => return Ok(Ended::Unwelcome),
            Err(Failure::Unreachable(fault)) => return Err(fault),
        };
        let Some(welcome) = self.greeted(&mut socket).await else {
            return Ok(Ended::Unwelcome);
        };
        log.info("uplink connected", &[]);
        let up = Instant::now();
        let offers = |code| welcome.capabilities.contains(&Capability::new(code));
        let mut link = Link {
            chats: ChatLink::new(
                self.chats.clone(),
                offers(UPLOAD),
                offers(LIVE),
                log.clone(),
            ),
            proposals: offers(PROPOSALS),
        };
        if self
            .serve(&mut socket, paired, from_daemon, &mut link)
            .await
        {
            return Ok(Ended::Unpaired);
        }
        let steady = up.elapsed() >= self.timing.steady_after;
        Ok(if steady {
            Ended::Lost
        } else {
            Ended::Unwelcome
        })
    }

    /// Carries messages both ways until the connection breaks, goes quiet or `paired` is no
    /// longer the pairing kept; `true` when it ends because the machine was unpaired.
    async fn serve(
        &self,
        socket: &mut Socket,
        paired: &Paired,
        from_daemon: &mut FromDaemon,
        link: &mut Link,
    ) -> bool {
        // Only what the server sends keeps the connection alive, and no send outlasts it.
        let mut quiet_at = Instant::now() + self.timing.quiet_within;
        let mut look_at = Instant::now() + self.timing.look_every;
        loop {
            let reply = tokio::select! {
                () = sleep_until(look_at) => {
                    if self.paired().await.as_ref() != Some(paired) {
                        return false;
                    }
                    look_at = Instant::now() + self.timing.look_every;
                    None
                }
                frame = timeout_at(quiet_at, socket.next()) => {
                    let Ok(Some(Ok(frame))) = frame else { return false };
                    quiet_at = Instant::now() + self.timing.quiet_within;
                    match self.answer(frame, &paired.owner, &mut link.chats) {
                        Step::Unpaired => return true,
                        Step::Reply(reply) => Some(*reply),
                        Step::Nothing => None,
                    }
                }
                Some(envelope) = from_daemon.recv() => link.offered(envelope, &self.to_daemon),
                Some(up) = link.chats.next() => Some(up),
            };
            if let Some(envelope) = reply
                && timeout_at(quiet_at, send(socket, &envelope)).await.is_err()
            {
                return false;
            }
        }
    }

    /// Says hello, and gives the server's welcome if it offers the connection.
    async fn greeted(&self, socket: &mut Socket) -> Option<Welcome> {
        let (offered, dir) = (self.capabilities, self.state_dir.clone());
        let capabilities = tokio::task::spawn_blocking(move || offered(&dir))
            .await
            .ok()?;
        let hello = Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol: PROTOCOL,
                app: self.app.clone(),
                capabilities,
            }),
        };
        send(socket, &hello).await;
        let Ok(Some(text)) = timeout(HELLO_WITHIN, first_text(socket)).await else {
            return None;
        };
        let Message::Welcome(welcome) = Envelope::from_json(text.as_bytes()).ok()?.message else {
            return None;
        };
        let connects = welcome.capabilities.contains(&Capability::new(CONNECT));
        connects.then_some(welcome)
    }
}

impl Uplink {
    /// What to do with a frame from the server. Only the machine's owner may ask it to act,
    /// whatever the server allows, and a kind whose asker it can't find is refused; the rest
    /// goes to the chats or the daemon.
    fn answer(&self, frame: Wire, owner: &AccountId, chats: &mut ChatLink) -> Step {
        let Wire::Text(text) = frame else {
            return Step::Nothing;
        };
        let Ok(envelope) = Envelope::from_json(text.as_bytes()) else {
            return Step::Nothing;
        };
        if let Message::Error(ErrorBody { code, .. }) = &envelope.message
            && *code == code!("machine.unpaired")
        {
            return Step::Unpaired;
        }
        let owners = match asking(&envelope.message) {
            Asking::By(by) => by == Some(owner),
            Asking::NotAnAsk => true,
            Asking::Unknown => return reply(failure(envelope.id, code!("uplink.unknown"))),
        };
        if !owners {
            if let Message::ChatCommand(command) = &envelope.message {
                return reply(refusal(envelope.id, command, code!("chat.not_owner")));
            }
            return reply(failure(envelope.id, code!("machine.not_owner")));
        }
        if ChatLink::is_for(&envelope.message) {
            return chats.take(envelope);
        }
        let id = envelope.id;
        match self.to_daemon.try_send(envelope) {
            Ok(()) => Step::Nothing,
            Err(_) => reply(for_now(id, code!("uplink.busy"))),
        }
    }
}

fn reply(envelope: Envelope) -> Step {
    Step::Reply(Box::new(envelope))
}

/// An error under the envelope `id` it answers.
pub(crate) const fn failure(id: u32, code: Code) -> Envelope {
    Envelope {
        id,
        message: Message::Error(ErrorBody {
            code,
            retryable: false,
        }),
    }
}

/// An error refusing the ask under the envelope `id` for now, so the server sends it again.
pub(crate) const fn for_now(id: u32, code: Code) -> Envelope {
    Envelope {
        id,
        message: Message::Error(ErrorBody {
            code,
            retryable: true,
        }),
    }
}

/// A receipt that fails `command` with `code`, under the envelope `id` it came in.
pub(crate) fn refusal(id: u32, command: &ChatCommand, code: Code) -> Envelope {
    Envelope {
        id,
        message: Message::Receipt(Receipt {
            request: command.request.clone(),
            outcome: Outcome::Failed { code },
        }),
    }
}

/// What one connection carries besides its socket.
struct Link {
    chats: ChatLink,
    /// Whether the server offered [`PROPOSALS`].
    proposals: bool,
}

impl Link {
    /// `envelope` from the daemon, unless the server doesn't take it: a proposal to a server
    /// that doesn't offer them is refused to the daemon at once.
    fn offered(&self, envelope: Envelope, to_daemon: &ToDaemon) -> Option<Envelope> {
        let refusal = proposals::unoffered(&envelope.message).filter(|_| !self.proposals);
        let Some(message) = refusal else {
            return Some(envelope);
        };
        drop(to_daemon.try_send(Envelope { id: 0, message }));
        None
    }
}

enum Step {
    Unpaired,
    Reply(Box<Envelope>),
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

/// The daemon's uplink, which takes the server's chat messages to `chats` and hands the
/// rest of what it asks to `to_daemon`.
pub(crate) fn new(
    app: &crate::app::App,
    state_dir: &Path,
    to_daemon: ToDaemon,
    chats: Chats,
) -> Uplink {
    Uplink {
        state_dir: state_dir.to_owned(),
        app: weacon_appkit::build(&app.version),
        capabilities: |dir| crate::session::offered(dir, &|key| std::env::var_os(key)),
        log: app.log.child(&[("component", "uplink".into())]),
        to_daemon,
        chats,
        timing: TIMING,
        random: weacon_appkit::random_id,
    }
}

#[cfg(test)]
mod tests {
    use tokio_rustls::server::TlsStream;
    use tokio_tungstenite::WebSocketStream;
    use weacon_protocol::{
        Asker, ChatAction, ChatId, ChatQuery, ChatQueryKind, MachineId, MachineSettingsAction,
        MachineSettingsCommand, MachineSettingsQuery, MachineSettingsQueryKind, MemberId,
        RequestId, RunEvent, SessionsQuery, SessionsQueryKind, TerminalAction, TerminalCommand,
        TerminalId, Welcome, WorkspaceId, WorktreeAction, WorktreeCommand, WorktreeQuery,
        WorktreeQueryKind,
    };
    use weacon_testing::TempDir;

    use super::*;
    use crate::chats::test_support::{brief, soon};
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

    fn with_commands(dir: &TempDir, to_daemon: ToDaemon) -> Uplink {
        Uplink {
            state_dir: dir.to_path_buf(),
            app: weacon_appkit::build(&VERSION),
            capabilities: |_| vec![Capability::new(CONNECT)],
            log: recording_logger().0,
            to_daemon,
            // A way to an engine that has stopped.
            chats: crate::chats::test_support::parts().0.chats,
            timing: Timing {
                quiet_within: Duration::from_secs(10),
                steady_after: Duration::ZERO,
                look_every: Duration::from_hours(1),
            },
            random: || 0,
        }
    }

    // One connection, with a daemon that sends nothing.
    async fn once(link: &Uplink, pairing: &Paired) -> Result<Ended, Fault> {
        link.connection(pairing, &link.log, &mut mpsc::channel(1).1)
            .await
    }

    fn text(envelope: &Envelope) -> Wire {
        Wire::text(String::from_utf8(envelope.to_json().unwrap()).unwrap())
    }

    // The core's next message; a core that leaves or goes quiet first fails the test.
    async fn heard(peer: &mut Peer) -> Message {
        let text = timeout(Duration::from_secs(10), next_text(peer))
            .await
            .unwrap();
        Envelope::from_json(text.as_bytes()).unwrap().message
    }

    async fn next_text(peer: &mut Peer) -> Utf8Bytes {
        loop {
            if let Wire::Text(text) = peer.next().await.unwrap().unwrap() {
                return text;
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
        assert_eq!(hello.capabilities, [Capability::new(CONNECT)]);
        let welcome = Welcome {
            app: weacon_appkit::build(&VERSION),
            capabilities: offers.iter().cloned().map(Capability::new).collect(),
        };
        let envelope = Envelope {
            id: 1,
            message: Message::Welcome(welcome),
        };
        peer.send(text(&envelope)).await.unwrap();
        peer
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

    #[tokio::test]
    async fn only_the_owners_commands_are_taken_and_an_unpairing_ends_it() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let pairing = paired(&tls);
        let stranger = AccountId::try_from("a-other".to_owned()).unwrap();
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT]).await;
            let sent = [
                command("r-owner", Some(&owner())),
                command("r-stranger", Some(&stranger)),
                command("r-nobody", None),
            ];
            peer.send_all(&mut futures_util::stream::iter(
                sent.iter().map(|asked| Ok(text(asked))),
            ))
            .await
            .unwrap();
            // The owner's interrupt reaches the chats, which have no such chat to stop.
            let heard_back = [
                heard(&mut peer).await,
                heard(&mut peer).await,
                heard(&mut peer).await,
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

        let (ended, heard_back) = tokio::join!(once(&link, &pairing), serve);

        assert_eq!(ended.unwrap(), Ended::Unpaired);
        let mut refusals: Vec<_> = heard_back.iter().filter_map(refusal).collect();
        refusals.sort_unstable();
        assert_eq!(
            refusals,
            [
                ("r-nobody", "chat.not_owner"),
                ("r-stranger", "chat.not_owner")
            ]
        );
        let errors = heard_back
            .iter()
            .filter(|heard| matches!(heard, Message::Error(_)));
        assert_eq!(errors.count(), 1, "{heard_back:?}");
    }

    fn the_owner() -> Asker {
        Asker {
            account: owner(),
            member: MemberId::try_from("m-1".to_owned()).unwrap(),
        }
    }

    // Has `agent` take a turn that calls a tool, says it is done and finishes.
    fn take_a_turn_with_a_tool(agent: &crate::chats::test_support::Started) {
        for said in a_turn_with_a_tool() {
            agent.say.send(said).unwrap();
        }
    }

    fn a_turn_with_a_tool() -> [RunEvent; 3] {
        let call = RunEvent::ToolCall {
            call: "t-1".to_owned(),
            tool: "Bash".to_owned(),
            input: serde_json::json!({ "command": "MARKER" }),
        };
        let finished = RunEvent::Finished {
            outcome: Outcome::Completed,
        };
        [call, crate::chats::test_support::text("Done"), finished]
    }

    // What the core sends up, in brief, until a run finishes.
    async fn until_finished(peer: &mut Peer) -> Vec<String> {
        let mut briefs = Vec::new();
        while briefs
            .last()
            .is_none_or(|brief: &String| !brief.contains("finished"))
        {
            briefs.push(brief(&heard(peer).await));
        }
        briefs
    }

    #[tokio::test]
    async fn a_server_that_stores_events_follows_a_chat_and_gets_its_messages_only() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let mut bench = crate::chats::test_support::bench();
        let link = Uplink {
            chats: bench.chats.clone(),
            ..uplink(&dir)
        };
        let pairing = paired(&tls);
        let follow = Envelope {
            id: 3,
            message: Message::ChatQuery(ChatQuery {
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                query: ChatQueryKind::Subscribe { after: 0 },
            }),
        };
        let prompt = Envelope {
            id: 4,
            message: Message::ChatCommand(ChatCommand {
                by: Some(the_owner()),
                ..crate::chats::test_support::prompt("r-1", "Hi")
            }),
        };
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT, code!("chat.upload")]).await;
            peer.send(text(&follow)).await.unwrap();
            peer.send(text(&prompt)).await.unwrap();
            let agent = soon(bench.started.recv()).await.unwrap();
            take_a_turn_with_a_tool(&agent);
            until_finished(&mut peer).await
        };

        let (ended, briefs) = tokio::join!(once(&link, &pairing), serve);

        assert_eq!(ended.unwrap(), Ended::Lost);
        // Seq 5, the tool's call, stays on the machine.
        assert_eq!(
            briefs,
            [
                "1 prompt r-1 Hi",
                "2 receipt r-1 accepted",
                "3 run-1 is Starting",
                "4 run-1 is Running",
                "6 run-1 text Done",
                "7 run-1 finished completed",
            ]
        );
    }

    // Each kind the server relays to the machine, asked `by` someone or no one.
    fn every_ask(by: Option<&AccountId>) -> Vec<Message> {
        let account = by.cloned();
        let by = by.map(|account| Asker {
            account: account.clone(),
            member: MemberId::try_from("m-1".to_owned()).unwrap(),
        });
        let request = RequestId::try_from("r-1".to_owned()).unwrap();
        let workspace = WorkspaceId::try_from("w-1".to_owned()).unwrap();
        let machine = MachineId::try_from("mc-1".to_owned()).unwrap();
        vec![
            Message::MachineSettingsQuery(MachineSettingsQuery {
                request: request.clone(),
                machine: machine.clone(),
                workspace: workspace.clone(),
                query: MachineSettingsQueryKind::Current,
                by: by.clone(),
            }),
            Message::MachineSettingsCommand(MachineSettingsCommand {
                request: request.clone(),
                machine: machine.clone(),
                workspace: workspace.clone(),
                action: MachineSettingsAction::SetReports { send: true },
                by: by.clone(),
            }),
            Message::SessionsQuery(SessionsQuery {
                request: request.clone(),
                machine: machine.clone(),
                query: SessionsQueryKind::List,
                by: account,
            }),
            Message::SymbolsQuery(weacon_protocol::SymbolsQuery {
                request,
                machine: machine.clone(),
                workspace: workspace.clone(),
                query: weacon_protocol::SymbolsQueryKind::Search {
                    words: "run".to_owned(),
                },
                by: by.clone(),
            }),
            Message::TerminalCommand(TerminalCommand {
                terminal: TerminalId::try_from("t-1".to_owned()).unwrap(),
                machine,
                workspace,
                action: TerminalAction::Close,
                by: by.clone(),
            }),
        ]
        .into_iter()
        .chain(worktree_asks(by))
        .collect()
    }

    fn worktree_asks(by: Option<Asker>) -> [Message; 3] {
        let request = RequestId::try_from("r-1".to_owned()).unwrap();
        let workspace = WorkspaceId::try_from("w-1".to_owned()).unwrap();
        let chat = ChatId::try_from("c-1".to_owned()).unwrap();
        [
            Message::DiffQuery(weacon_protocol::DiffQuery {
                request: request.clone(),
                workspace: workspace.clone(),
                chat: chat.clone(),
                query: weacon_protocol::DiffQueryKind::Changes,
                by: by.clone(),
            }),
            Message::WorktreeQuery(WorktreeQuery {
                request: request.clone(),
                workspace: workspace.clone(),
                chat: chat.clone(),
                query: WorktreeQueryKind::State,
                by: by.clone(),
            }),
            Message::WorktreeCommand(WorktreeCommand {
                request,
                workspace,
                chat,
                action: WorktreeAction::OpenTerminal,
                by,
            }),
        ]
    }

    // What the uplink answers `message` under id 9 with: `None` when it passes it on.
    fn answered(link: &Uplink, message: Message) -> Option<String> {
        let frame = text(&Envelope { id: 9, message });
        let mut chats = ChatLink::new(link.chats.clone(), false, false, link.log.clone());
        let reply = match link.answer(frame, &owner(), &mut chats) {
            Step::Nothing => return None,
            Step::Unpaired => return Some("unpaired".to_owned()),
            Step::Reply(reply) => *reply,
        };
        match reply {
            Envelope {
                id: 9,
                message: Message::Error(ErrorBody { code, .. }),
            } => Some(code.as_str().to_owned()),
            other => Some(format!("{other:?}")),
        }
    }

    #[tokio::test]
    async fn every_ask_but_the_owners_and_every_kind_it_cant_place_is_refused() {
        let dir = TempDir::new().unwrap();
        let (to_daemon, mut taken) = mpsc::channel(16);
        let link = with_commands(&dir, to_daemon);
        let stranger = AccountId::try_from("a-other".to_owned()).unwrap();
        let not_owner = Some("machine.not_owner".to_owned());
        let other_error = Message::Error(ErrorBody {
            code: code!("relay.busy"),
            retryable: true,
        });

        let owners: Vec<_> = every_ask(Some(&owner()))
            .into_iter()
            .map(|ask| answered(&link, ask))
            .collect();
        let others: Vec<_> = [every_ask(Some(&stranger)), every_ask(None)]
            .concat()
            .into_iter()
            .map(|ask| answered(&link, ask))
            .collect();
        let error = answered(&link, other_error.clone());
        // A server that doesn't store chats' events may not follow one.
        let chat_query = answered(
            &link,
            Message::ChatQuery(ChatQuery {
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                query: ChatQueryKind::Subscribe { after: 0 },
            }),
        );

        assert_eq!(owners, vec![None; 8]);
        assert_eq!(others, vec![not_owner; 16]);
        // A kind this build doesn't know arrives as `Message::Unknown`.
        assert!(matches!(asking(&Message::Unknown), Asking::Unknown));
        assert_eq!(error, None);
        assert_eq!(chat_query, Some("message.unsupported".to_owned()));
        let passed: Vec<_> = std::iter::from_fn(|| taken.try_recv().ok())
            .map(|envelope| envelope.message)
            .collect();
        let mut expected = every_ask(Some(&owner()));
        expected.push(other_error);
        assert_eq!(passed, expected);
    }

    #[tokio::test]
    async fn an_owners_ask_reaches_the_daemon_and_its_own_messages_reach_the_server() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let (to_daemon, mut taken) = mpsc::channel(4);
        let link = with_commands(&dir, to_daemon);
        let pairing = paired(&tls);
        let (outgoing, mut from_daemon) = mpsc::channel(4);
        let query = Envelope {
            id: 5,
            message: every_ask(Some(&owner())).remove(0),
        };
        let answer = Envelope {
            id: 5,
            message: Message::Receipt(Receipt {
                request: RequestId::try_from("r-1".to_owned()).unwrap(),
                outcome: Outcome::Accepted,
            }),
        };
        outgoing.send(answer.clone()).await.unwrap();
        let serve = async {
            let mut peer = welcoming(&tls, &[CONNECT]).await;
            peer.send(text(&query)).await.unwrap();
            heard(&mut peer).await
        };

        let (ended, heard_back) = tokio::join!(
            link.connection(&pairing, &link.log, &mut from_daemon),
            serve
        );

        assert_eq!(ended.unwrap(), Ended::Lost);
        assert_eq!(heard_back, answer.message);
        assert_eq!(taken.try_recv().unwrap(), query);
    }

    #[tokio::test]
    async fn an_ask_the_daemon_has_no_room_for_is_refused_for_now() {
        let dir = TempDir::new().unwrap();
        let (to_daemon, _taken) = mpsc::channel(1);
        let link = with_commands(&dir, to_daemon);
        let ask = || every_ask(Some(&owner())).remove(0);
        assert_eq!(answered(&link, ask()), None);

        let frame = text(&Envelope {
            id: 9,
            message: ask(),
        });
        let mut chats = ChatLink::new(link.chats.clone(), false, false, link.log.clone());
        let Step::Reply(reply) = link.answer(frame, &owner(), &mut chats) else {
            panic!("not refused");
        };

        let busy = ErrorBody {
            code: code!("uplink.busy"),
            retryable: true,
        };
        assert_eq!(
            *reply,
            Envelope {
                id: 9,
                message: Message::Error(busy)
            }
        );
    }

    #[tokio::test]
    async fn a_connection_that_breaks_is_lost() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let pairing = paired(&tls);

        let (ended, ()) = tokio::join!(once(&link, &pairing), async {
            drop(welcoming(&tls, &[CONNECT]).await);
        });

        assert_eq!(ended.unwrap(), Ended::Lost);
    }

    #[tokio::test]
    async fn a_server_that_doesnt_offer_the_connection_or_refuses_the_token_is_left() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let (log, records) = recording_logger();
        let link = Uplink {
            log,
            ..uplink(&dir)
        };
        let pairing = paired(&tls);

        // The server goes once it has welcomed the core, which then mustn't stay.
        let leaves = async { drop(welcoming(&tls, &[]).await) };
        let (unoffered, ()) = tokio::join!(once(&link, &pairing), leaves);
        let (unauthorized, ()) = tokio::join!(
            once(&link, &pairing),
            refusing(&tls, "401 Unauthorized", code!("machine.unpaired"))
        );
        let (busy, ()) = tokio::join!(
            once(&link, &pairing),
            refusing(&tls, "401 Unauthorized", code!("proxy.sign_in"))
        );

        assert_eq!(unoffered.unwrap(), Ended::Unwelcome);
        assert_eq!(unauthorized.unwrap(), Ended::Unpaired);
        assert_eq!(busy.unwrap(), Ended::Unwelcome);
        assert!(!messages(&records).contains(&"uplink connected"));
    }

    #[tokio::test]
    async fn a_connection_lost_at_once_or_gone_quiet_is_told_apart() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let mut link = uplink(&dir);
        link.timing = Timing {
            quiet_within: Duration::from_millis(100),
            steady_after: Duration::from_hours(1),
            look_every: Duration::from_hours(1),
        };
        let pairing = paired(&tls);

        let (at_once, ()) = tokio::join!(once(&link, &pairing), async {
            drop(welcoming(&tls, &[CONNECT]).await);
        });
        link.timing.steady_after = Duration::ZERO;
        let offers = [CONNECT];
        let (quiet, _peer) = tokio::join!(once(&link, &pairing), welcoming(&tls, &offers));

        assert_eq!(at_once.unwrap(), Ended::Unwelcome);
        assert_eq!(quiet.unwrap(), Ended::Lost);
    }

    // Reads all the core sends, saying nothing, until it leaves.
    async fn drain(mut peer: Peer) {
        while let Some(Ok(_)) = peer.next().await {}
    }

    // A daemon that sends to the server for as long as it can.
    async fn chatter(outgoing: mpsc::Sender<Envelope>) {
        let receipt = Envelope {
            id: 5,
            message: Message::Receipt(Receipt {
                request: RequestId::try_from("r-1".to_owned()).unwrap(),
                outcome: Outcome::Accepted,
            }),
        };
        while outgoing.send(receipt.clone()).await.is_ok() {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_server_gone_quiet_is_lost_however_much_the_daemon_sends() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let mut link = uplink(&dir);
        link.timing = Timing {
            quiet_within: Duration::from_millis(100),
            steady_after: Duration::ZERO,
            look_every: Duration::from_hours(1),
        };
        let pairing = paired(&tls);
        let (outgoing, mut from_daemon) = mpsc::channel(4);
        let chatter = tokio::spawn(chatter(outgoing));

        // The server welcomes the core, then reads all it sends but says nothing more.
        let connection = link.connection(&pairing, &link.log, &mut from_daemon);
        let listening = async { drain(welcoming(&tls, &[CONNECT]).await).await };
        let (ended, ()) = tokio::join!(timeout(Duration::from_secs(10), connection), listening);
        chatter.abort();

        assert_eq!(ended.unwrap().unwrap(), Ended::Lost);
    }

    #[tokio::test]
    async fn a_pairing_replaced_while_connected_ends_the_connection() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let mut link = uplink(&dir);
        link.timing.look_every = Duration::from_millis(10);
        link.timing.quiet_within = Duration::from_hours(1);
        let pairing = paired(&tls);
        machine::write(&dir, &pairing).unwrap();
        let mut replaced = pairing.clone();
        replaced.token = "n3wer".to_owned();

        // The server stays and says nothing, while `weacon pair` keeps another pairing.
        let (_daemon, mut from_daemon) = mpsc::channel(1);
        let connection = link.connection(&pairing, &link.log, &mut from_daemon);
        let listening = async {
            let peer = welcoming(&tls, &[CONNECT]).await;
            machine::write(&dir, &replaced).unwrap();
            drain(peer).await;
        };
        let (ended, ()) = tokio::join!(timeout(Duration::from_secs(10), connection), listening);

        assert_eq!(ended.unwrap().unwrap(), Ended::Lost);
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
                capabilities: vec![Capability::new(CONNECT)],
            };
            let envelope = Envelope {
                id: 1,
                message: Message::Welcome(welcome),
            };
            peer.send(text(&envelope)).await.unwrap();
        };

        let (ended, ()) = tokio::join!(once(&link, &pairing), serve);

        assert_eq!(ended.unwrap(), Ended::Lost);
    }

    #[tokio::test]
    async fn after_an_unpairing_or_without_a_pairing_the_core_waits_to_look_again() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let link = uplink(&dir);
        let mut retry = Retry::default();
        let mut from_daemon = mpsc::channel(1).1;
        machine::write(&dir, &paired(&tls)).unwrap();

        let (unpaired, ()) = tokio::join!(
            link.step(&mut retry, &mut from_daemon),
            refusing(&tls, "401 Unauthorized", code!("machine.unpaired"))
        );
        let alone = link.step(&mut retry, &mut from_daemon).await;

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

        let failed = once(&link, &gone).await;

        assert_eq!(failed.unwrap_err().code().as_str(), "server.unreachable");
    }

    /// What the server hears first when the daemon sends a proposal query, then a marker, to
    /// a server that offers `offers`, and what reaches the daemon of the server's answer.
    async fn proposals_through(offers: &[Code]) -> (Message, Option<Message>, Message) {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let (to_daemon, mut asks) = mpsc::channel(4);
        let link = with_commands(&dir, to_daemon);
        let pairing = paired(&tls);
        let request = RequestId::try_from("r-1".to_owned()).unwrap();
        let query = Message::ProposalQuery(weacon_protocol::ProposalQuery {
            request: request.clone(),
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            query: weacon_protocol::ProposalQueryKind::Pinned { after: None },
        });
        let marker = Message::Error(ErrorBody {
            code: code!("test.marker"),
            retryable: false,
        });
        let (from_daemon, mut sent) = mpsc::channel(4);
        for message in [query, marker] {
            from_daemon.send(Envelope { id: 0, message }).await.unwrap();
        }
        let answer = Message::ProposalAnswer(weacon_protocol::ProposalAnswer {
            request,
            answer: weacon_protocol::ProposalAnswerKind::Pinned {
                docs: Vec::new(),
                more: false,
            },
        });
        let serve = async {
            let mut peer = welcoming(&tls, offers).await;
            let first = heard(&mut peer).await;
            let envelope = Envelope {
                id: 0,
                message: answer.clone(),
            };
            peer.send(text(&envelope)).await.unwrap();
            let handed = timeout(Duration::from_secs(10), asks.recv()).await.unwrap();
            (first, handed.map(|envelope| envelope.message))
        };
        let (_, (first, handed)) =
            tokio::join!(link.connection(&pairing, &link.log, &mut sent), serve);
        (first, handed, answer)
    }

    #[tokio::test]
    async fn proposals_reach_only_a_server_that_offers_them_and_its_answers_reach_the_daemon() {
        let (unoffered, handed, _) = Box::pin(proposals_through(&[CONNECT])).await;
        assert!(matches!(unoffered, Message::Error(_)), "{unoffered:?}");
        let refused = weacon_protocol::ProposalAnswerKind::Failed {
            code: code!("proposal.unsupported"),
        };
        assert!(
            matches!(&handed, Some(Message::ProposalAnswer(answer)) if answer.answer == refused),
            "{handed:?}"
        );

        let (offered, handed, answer) = Box::pin(proposals_through(&[CONNECT, PROPOSALS])).await;
        assert!(matches!(offered, Message::ProposalQuery(_)), "{offered:?}");
        assert_eq!(handed, Some(answer));
    }
}
