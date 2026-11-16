use std::fs::{self, File, TryLockError};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
pub(crate) use weacon_appkit::build;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{App, Envelope, LineDecoder, code, encode_line};

use crate::chats::feed::{self, Client};
use crate::chats::{self, Chats, Places};
use crate::git::service::Worktrees;
use crate::mcp;
use crate::outside::Outside;
use crate::picks::Picks;
use crate::platform::{self, Listener, Stream};
use crate::proposals;
use crate::session::Session;

mod chat;
mod route;
mod worktrees;

pub(crate) const READ_CHUNK: usize = 8192;
pub(crate) const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const PICKS_SWEEP: Duration = Duration::from_mins(1);

// Removes the socket a daemon that stopped left behind, if there is one.
fn remove_stale(socket: &Path) -> io::Result<()> {
    match fs::remove_file(socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

pub(crate) fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("weacon.sock")
}

/// Runs the daemon until Ctrl-C or the system asks it to stop.
pub(crate) fn run(app: &crate::app::App) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    crate::app::runtime()?.block_on(async {
        let signals = weacon_appkit::Signals::new().map_err(|error| {
            Fault::new(
                code!("daemon.signals_failed"),
                "The daemon couldn't catch signals",
            )
            .because(error)
        })?;
        let mut daemon = Daemon::start(
            state_dir,
            build(&app.version),
            Places {
                path: app.path.clone(),
                work_root: app.work_root.clone(),
                mcp: None,
                plugin: None,
            },
            app.log.clone(),
        )?;
        daemon.shared.outside = app.outside.clone();
        let (chats, worktrees) = (daemon.shared.chats.clone(), daemon.shared.worktrees.clone());
        let maps = daemon.shared.maps.clone();
        if let Some(proposing) = daemon.proposing.take() {
            let server = route::server(app, state_dir, chats, worktrees, (maps, proposing));
            daemon.uplink.spawn(server);
        }
        daemon.serve(signals.wait()).await;
        Ok(())
    })
}

#[derive(Clone)]
struct Shared {
    app: App,
    picks: Picks,
    chats: Chats,
    worktrees: Worktrees,
    outside: Outside,
    mcp: mcp::Mcp,
    maps: mcp::Maps,
    log: Logger,
}

/// A daemon that holds its state folder's lock and listens on its socket.
pub(crate) struct Daemon {
    listener: Listener,
    socket: PathBuf,
    shared: Shared,
    engine: JoinHandle<()>,
    uplink: JoinSet<()>,
    /// What the MCP and the map's watches propose, until the uplink takes it.
    proposing: Option<proposals::Asks>,
    mcp: mcp::Server,
    // Held for the daemon's life, so a second daemon can't start.
    _lock: File,
}

impl Daemon {
    /// Locks `state_dir`, replaces a stale socket and listens; must run inside a tokio runtime.
    pub(crate) fn start(
        state_dir: &Path,
        app: App,
        mut places: Places,
        log: Logger,
    ) -> Result<Self, Fault> {
        let failed = |error: io::Error| {
            Fault::new(code!("daemon.start_failed"), "The daemon couldn't start").because(error)
        };
        // The socket, the pairing and the plugin its agents run are in it.
        platform::own_dir(state_dir).map_err(failed)?;
        let lock = weacon_appkit::private_append(&state_dir.join("weacon.lock")).map_err(failed)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => Fault::new(
                code!("daemon.running"),
                "Another daemon is already running for this state folder",
            ),
            TryLockError::Error(error) => failed(error),
        })?;
        let socket = socket_path(state_dir);
        remove_stale(&socket).map_err(failed)?;
        // Only under the lock: another daemon's runs may be reading its own.
        places.plugin = crate::integrations::Plugin::new(state_dir, &app.version, &log);
        let (proposals, proposing) = proposals::channel();
        let maps = mcp::Maps::new(state_dir, &places).proposing(proposals);
        let (mcp, mcp_server) = mcp::start(&app, &log, maps.clone())?;
        places.mcp = Some(mcp.clone());
        let worktrees = worktrees::worktrees(state_dir, &places);
        let (chats, engine) = chats::start(state_dir, places, app.clone(), log.clone())?;
        let listener = platform::bind(&socket).map_err(failed)?;
        log.info("daemon started", &[]);
        Ok(Self {
            listener,
            socket,
            uplink: JoinSet::new(),
            proposing: Some(proposing),
            engine: tokio::spawn(engine.serve()),
            shared: Shared {
                app,
                picks: Picks::new(state_dir),
                chats,
                worktrees,
                outside: Outside::default(),
                mcp,
                maps,
                log,
            },
            mcp: mcp_server,
            _lock: lock,
        })
    }

    /// Serves clients until `shutdown` finishes, then closes every connection and the socket.
    pub(crate) async fn serve(mut self, shutdown: impl Future<Output = ()>) {
        let mut clients = JoinSet::new();
        let mut picks_sweep = tokio::time::interval(PICKS_SWEEP);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                _ = picks_sweep.tick() => route::sweep(&self.shared).await,
                accepted = self.listener.accept() => match accepted {
                    Ok(stream) => self.admit(stream, &mut clients),
                    Err(error) => {
                        // Such as too many open files: wait, rather than spin and flood the log.
                        self.shared.log.warn("accept failed", &[("err", Field::error(&error))]);
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
        drop(self.listener);
        drop(fs::remove_file(&self.socket));
        clients.shutdown().await;
        // The uplink's way to the engine goes first, or the engine would never stop.
        self.uplink.shutdown().await;
        // Worktree work outlives its asker; it ends before the lock goes.
        self.shared.worktrees.drain().await;
        drop(self.mcp);
        // The engine stops once the last way to it is gone; its writes end before the lock goes.
        let log = self.shared.log.clone();
        drop(self.shared);
        if self.engine.await.is_err() {
            log.error("engine failed", &[]);
        }
        log.info("daemon stopped", &[]);
    }

    #[cfg(test)]
    pub(crate) const fn mcp(&self) -> &mcp::Mcp {
        &self.shared.mcp
    }

    fn admit(&self, stream: Stream, clients: &mut JoinSet<()>) {
        let shared = self.shared.clone();
        clients.spawn(async move {
            if let Err(error) = serve_client(stream, &shared).await {
                shared
                    .log
                    .debug("client failed", &[("err", Field::error(&error))]);
            }
            shared.chats.left().await;
        });
    }
}

/// One client's connection: where it is in the conversation, and where events it subscribed to
/// are pushed.
struct Connection {
    session: Session,
    decoder: LineDecoder,
    client: Client,
    late: worktrees::Late,
}

async fn serve_client(stream: Stream, shared: &Shared) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = [0; READ_CHUNK];
    let (push, mut pushed) = mpsc::channel(feed::QUEUE);
    let (late, mut answered) = mpsc::channel(worktrees::LATE);
    let mut connection = Connection {
        session: Session::default(),
        decoder: LineDecoder::default(),
        client: Client { push },
        late,
    };
    loop {
        let read = tokio::select! {
            read = reader.read(&mut buf) => read?,
            Some(message) = pushed.recv() => {
                if let Ok(line) = encode_line(&Envelope { id: 0, message }) {
                    writer.write_all(&line).await?;
                }
                continue;
            }
            Some(envelope) = answered.recv() => {
                if let Ok(line) = encode_line(&envelope) {
                    writer.write_all(&line).await?;
                }
                continue;
            }
        };
        if read == 0 {
            return Ok(());
        }
        let bytes = buf.get(..read).unwrap_or_default();
        if !answer(&mut connection, bytes, shared, &mut writer).await? {
            return Ok(());
        }
    }
}

// Answers what `bytes` completes; false once the connection should close.
async fn answer(
    connection: &mut Connection,
    bytes: &[u8],
    shared: &Shared,
    writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<bool> {
    let Connection {
        session,
        decoder,
        client,
        late,
    } = connection;
    for received in decoder.push(bytes) {
        let reply = route::route(shared, session, received, client, late).await;
        if let Some(envelope) = reply.envelope
            && let Ok(line) = encode_line(&envelope)
        {
            writer.write_all(&line).await?;
        }
        if reply.close {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncRead};
    use tokio::sync::oneshot;
    use tokio::time::timeout;
    use weacon_protocol::{Capability, ChatId, Message, Outcome, PROTOCOL, Welcome, WorkspaceId};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, Records, VERSION, error_code, messages, recording_logger};

    struct Running {
        socket: PathBuf,
        stop: oneshot::Sender<()>,
        served: JoinHandle<()>,
        records: Records,
    }

    impl Running {
        async fn stop(self) -> Records {
            self.stop.send(()).unwrap();
            self.served.await.unwrap();
            self.records
        }
    }

    fn start(dir: &Path) -> Result<Running, Fault> {
        start_as(dir, |_| {})
    }

    // Starts a daemon, letting `change` adjust it before it serves.
    fn start_as(dir: &Path, change: impl FnOnce(&mut Daemon)) -> Result<Running, Fault> {
        let (log, records) = recording_logger();
        let mut daemon = Daemon::start(dir, build(&VERSION), Places::default(), log)?;
        change(&mut daemon);
        let socket = daemon.socket.clone();
        let (stop, stopped) = oneshot::channel();
        let served = tokio::spawn(daemon.serve(async move { drop(stopped.await) }));
        Ok(Running {
            socket,
            stop,
            served,
            records,
        })
    }

    async fn ask(socket: &Path) -> Result<Welcome, Fault> {
        weacon_client::handshake(socket, build(&VERSION)).await
    }

    fn hello(protocol: u32) -> Vec<u8> {
        encode_line(&test_support::client_hello(1, protocol, [])).unwrap()
    }

    // Sends `bytes` and reads replies until the daemon hangs up or `count` arrive.
    async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
        stream: S,
        bytes: &[u8],
        count: usize,
    ) -> (Vec<Message>, S) {
        let (reader, mut writer) = tokio::io::split(stream);
        writer.write_all(bytes).await.unwrap();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let mut replies = Vec::new();
        // So a daemon that never answers fails the test.
        let wait = Duration::from_secs(10);
        while replies.len() < count
            && let Some(line) = timeout(wait, lines.next_line()).await.unwrap().unwrap()
        {
            replies.push(serde_json::from_str::<Envelope>(&line).unwrap().message);
        }
        let reader = lines.into_inner().into_inner();
        (replies, reader.unsplit(writer))
    }

    async fn connect(socket: &Path) -> impl AsyncRead + AsyncWrite + Unpin + use<> {
        weacon_client::connect(socket).await.unwrap()
    }

    #[tokio::test]
    async fn a_client_shakes_hands_and_shutdown_cleans_up() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let socket = running.socket.clone();

        assert_eq!(ask(&socket).await.unwrap().app, build(&VERSION));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let records = running.stop().await;
        assert!(!socket.exists());
        assert_eq!(
            ask(&socket).await.unwrap_err().code().as_str(),
            "daemon.not_running"
        );
        let lifecycle: Vec<_> = messages(&records)
            .into_iter()
            .filter(|message| message.starts_with("daemon "))
            .collect();
        assert_eq!(lifecycle, ["daemon started", "daemon stopped"]);
    }

    #[test]
    fn a_socket_that_cant_be_removed_is_an_error_and_none_there_is_fine() {
        let dir = TempDir::new().unwrap();
        let held = dir.join("held");
        fs::create_dir(&held).unwrap();

        remove_stale(&dir.join("gone")).unwrap();
        remove_stale(&held).unwrap_err();
    }

    #[tokio::test]
    async fn a_paired_daemon_dials_its_server_and_still_stops() {
        use crate::machine::{self, Key, Paired};
        let (dir, tls) = (TempDir::new().unwrap(), test_support::tls::Tls::new().await);
        let paired = Paired {
            server: tls.url(),
            pinned: Some(tls.pem.clone()),
            machine: "mc-1".parse().unwrap(),
            owner: "a-owner".parse().unwrap(),
            token: "t0ken".to_owned(),
            key: Key::from_random([7; 32]),
        };
        machine::write(&dir, &paired).unwrap();
        let app = test_support::app(Some(&dir), None);
        let running = start_as(&dir, |daemon| {
            let (chats, worktrees) = (daemon.shared.chats.clone(), daemon.shared.worktrees.clone());
            let proposing = daemon.proposing.take().unwrap();
            let maps = daemon.shared.maps.clone();
            let serving = (maps, proposing);
            daemon
                .uplink
                .spawn(route::server(&app, &dir, chats, worktrees, serving));
        })
        .unwrap();

        let dialed = tls.accept().await;

        assert!(dialed.is_some(), "the daemon never dialed its server");
        // Its uplink, which holds a way to the engine, must not hold the engine up.
        let stopped = timeout(Duration::from_secs(10), running.stop()).await;
        assert!(stopped.is_ok(), "the daemon never stopped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_state_folder_others_can_enter_is_made_its_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        let running = start(&dir).unwrap();

        let mode = fs::metadata(&*dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        running.stop().await;
    }

    #[tokio::test]
    async fn one_daemon_per_state_folder_and_a_stale_socket_is_replaced() {
        let dir = TempDir::new().unwrap();
        fs::write(socket_path(&dir), "stale").unwrap();
        let first = start(&dir).unwrap();
        let second = start(&dir).err().unwrap();

        assert_eq!(second.code().as_str(), "daemon.running");
        let hooks = dir.join("claude-plugin/hooks/hooks.json");
        fs::write(&hooks, "the first daemon's").unwrap();
        start(&dir).err().unwrap();
        assert_eq!(fs::read_to_string(&hooks).unwrap(), "the first daemon's");
        ask(&first.socket).await.unwrap();
        first.stop().await;
    }

    #[tokio::test]
    async fn bad_frames_get_errors_and_the_connection_goes_on() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let mut bytes = b"not json\n".to_vec();
        bytes.extend(hello(PROTOCOL));
        let (replies, client) = exchange(connect(&running.socket).await, &bytes, 2).await;
        let (later, _) = exchange(client, b"[]\n", 1).await;

        assert_eq!(error_code(&replies[0]), Some("frame.invalid"));
        assert!(matches!(replies[1], Message::Welcome(_)));
        assert_eq!(error_code(&later[0]), Some("frame.invalid"));
        running.stop().await;
    }

    // In real time: a hang-up can take a moment to arrive, which a paused clock would skip past.
    #[tokio::test]
    async fn another_protocol_is_hung_up_on_at_once() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let other = hello(PROTOCOL + 1);
        let talk = exchange(connect(&running.socket).await, &other, 9);
        let (replies, _) = timeout(Duration::from_secs(3), talk).await.unwrap();

        assert_eq!(replies.len(), 1);
        assert_eq!(error_code(&replies[0]), Some("protocol.unsupported"));
        running.stop().await;
    }

    #[tokio::test]
    async fn shutdown_hangs_up_on_connected_clients() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let (replies, mut client) =
            exchange(connect(&running.socket).await, &hello(PROTOCOL), 1).await;
        running.stop().await;

        assert!(matches!(replies[0], Message::Welcome(_)));
        assert_eq!(client.read(&mut [0; 16]).await.unwrap(), 0);
    }

    fn pick(id: u32, request: &str, html: &str) -> Vec<u8> {
        let message = Message::Pick(test_support::pick(request, html));
        encode_line(&Envelope { id, message }).unwrap()
    }

    fn receipt(message: &Message) -> Option<&Outcome> {
        let Message::Receipt(receipt) = message else {
            return None;
        };
        Some(&receipt.outcome)
    }

    #[tokio::test]
    async fn a_pick_is_kept_and_answered_with_a_receipt() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let mut bytes = hello(PROTOCOL);
        bytes.extend(pick(2, "p-1", "<b>MARKER-HTML</b>"));
        bytes.extend(pick(3, "p-1", "<b>MARKER-HTML</b>"));
        bytes.extend(pick(4, "p-1", "<i>other</i>"));
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 4).await;
        let records = running.stop().await;
        let kept = Picks::new(&dir).read(&"p-1".to_owned().try_into().unwrap());

        assert!(matches!(&replies[0], Message::Welcome(welcome)
            if welcome.capabilities.contains(&Capability::new(code!("element.pick")))));
        assert_eq!(receipt(&replies[1]), Some(&Outcome::Completed));
        assert_eq!(receipt(&replies[2]), Some(&Outcome::Completed));
        assert_eq!(error_code(&replies[3]), Some("request.conflict"));
        assert_eq!(kept.unwrap().unwrap().html, "<b>MARKER-HTML</b>");
        let picked = messages(&records);
        assert_eq!(
            picked
                .iter()
                .filter(|msg| **msg == "element picked")
                .count(),
            2
        );
        let logged = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(logged.contains("p-1"), "{logged}");
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_pick_that_cant_be_kept_fails_its_receipt() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        fs::write(dir.join("picks"), "a file, not a folder").unwrap();
        let mut bytes = hello(PROTOCOL);
        bytes.extend(pick(2, "p-2", "<p/>"));
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 2).await;
        let records = running.stop().await;

        assert_eq!(
            receipt(&replies[1]),
            Some(&Outcome::Failed {
                code: code!("pick.not_kept")
            })
        );
        assert!(messages(&records).contains(&"pick not kept"));
        assert!(messages(&records).contains(&"picks not swept"));
    }

    // Writes a runs journal with a waiting chat in `w-1` for each of `chats`: its id, its agent,
    // its run as JSON and when it was last active.
    fn journal(dir: &Path, chats: &[(&str, &str, &str, u32)]) {
        let records: Vec<_> = chats
            .iter()
            .map(|(chat, agent, run, at)| {
                format!(
                    r#"{{"workspace":"w-1","chat":"{chat}","agent":"{agent}","run":{run},"state":"waiting","session":null,"turn":null,"queue":[],"active_at":{at}}}"#
                )
            })
            .collect();
        let journal = format!(r#"{{"issued":2,"chats":[{}]}}"#, records.join(","));
        fs::write(dir.join("runs.json"), journal).unwrap();
    }

    #[tokio::test]
    async fn the_chats_served_here_are_listed_newest_first_with_their_titles() {
        use weacon_protocol::{
            AgentKind, ChatEvent, ChatsAnswer, ChatsAnswerKind, ChatsQuery, ChatsQueryKind,
            LocalChat, RunState,
        };

        use crate::chats::log::ChatLog;

        let dir = TempDir::new().unwrap();
        let workspace: WorkspaceId = "w-1".parse().unwrap();
        let root = crate::workspaces::Workspaces::new(&dir, None).root(&workspace);
        let mut log = ChatLog::open(&root, &"c-1".parse().unwrap()).await.unwrap();
        let text = "Fix the header\nand the footer".to_owned();
        log.append(ChatEvent::OutsidePrompt { text }).await.unwrap();
        journal(
            &dir,
            &[
                ("c-1", "claude", "null", 10),
                ("c-2", "codex", r#""run-2""#, 20),
            ],
        );
        let running = start(&dir).unwrap();
        let list = |id, query| {
            let message = Message::ChatsQuery(ChatsQuery { query });
            encode_line(&Envelope { id, message }).unwrap()
        };
        let mut bytes = hello(PROTOCOL);
        bytes.extend(list(2, ChatsQueryKind::List));
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 2).await;
        running.stop().await;

        let chat = |id: &str, title: &str, agent, updated_at, run| LocalChat {
            workspace: workspace.clone(),
            chat: id.parse().unwrap(),
            title: title.to_owned(),
            agent,
            updated_at,
            run,
        };
        let chats = vec![
            chat("c-2", "", AgentKind::Codex, 20, Some(RunState::Sleeping)),
            chat("c-1", "Fix the header", AgentKind::Claude, 10, None),
        ];
        assert_eq!(
            replies[1],
            Message::ChatsAnswer(ChatsAnswer {
                answer: ChatsAnswerKind::List { chats }
            })
        );
    }

    #[tokio::test]
    async fn a_chat_whose_log_cant_be_read_is_listed_untitled() {
        use weacon_protocol::{ChatsAnswerKind, ChatsQuery, ChatsQueryKind};

        let dir = TempDir::new().unwrap();
        let root = crate::workspaces::Workspaces::new(&dir, None).root(&"w-1".parse().unwrap());
        fs::create_dir_all(root.join("chats/c-c-1.jsonl")).unwrap();
        journal(&dir, &[("c-1", "claude", "null", 1)]);
        let running = start(&dir).unwrap();
        let message = Message::ChatsQuery(ChatsQuery {
            query: ChatsQueryKind::List,
        });
        let mut bytes = hello(PROTOCOL);
        bytes.extend(encode_line(&Envelope { id: 2, message }).unwrap());
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 2).await;
        let records = running.stop().await;

        let Message::ChatsAnswer(answer) = &replies[1] else {
            panic!("not a list: {:?}", replies[1]);
        };
        let ChatsAnswerKind::List { chats } = &answer.answer;
        assert_eq!(
            chats
                .iter()
                .map(|chat| chat.title.as_str())
                .collect::<Vec<_>>(),
            [""]
        );
        assert!(messages(&records).contains(&"chat titles unreadable"));
    }

    #[tokio::test]
    async fn a_pick_before_the_hello_is_refused() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let (replies, _) =
            exchange(connect(&running.socket).await, &pick(1, "p-3", "<p/>"), 1).await;
        running.stop().await;

        assert_eq!(error_code(&replies[0]), Some("handshake.required"));
        assert!(!dir.join("picks").exists());
    }

    // A diff of `chat`, then a diff, a state query and a removal of `c-2`, which has no worktree.
    fn worktree_asks(workspace: &WorkspaceId, chat: &ChatId) -> Vec<u8> {
        use weacon_protocol::{
            DiffQuery, DiffQueryKind, RequestId, WorktreeAction, WorktreeCommand, WorktreeQuery,
            WorktreeQueryKind,
        };

        let line = |id, message| encode_line(&Envelope { id, message }).unwrap();
        let request = RequestId::try_from("r-1".to_owned()).unwrap();
        let other = ChatId::try_from("c-2".to_owned()).unwrap();
        let diff = |chat: &ChatId| DiffQuery {
            request: request.clone(),
            workspace: workspace.clone(),
            chat: chat.clone(),
            query: DiffQueryKind::Changes,
            by: None,
        };
        let query = WorktreeQuery {
            request: request.clone(),
            workspace: workspace.clone(),
            chat: other.clone(),
            query: WorktreeQueryKind::State,
            by: None,
        };
        let command = WorktreeCommand {
            request: request.clone(),
            workspace: workspace.clone(),
            chat: other.clone(),
            action: WorktreeAction::Remove,
            by: None,
        };
        [
            hello(PROTOCOL),
            line(2, Message::DiffQuery(diff(chat))),
            line(3, Message::DiffQuery(diff(&other))),
            line(4, Message::WorktreeQuery(query)),
            line(5, Message::WorktreeCommand(command)),
        ]
        .concat()
    }

    fn diff_answer(message: &Message) -> Option<&weacon_protocol::DiffAnswerKind> {
        let Message::DiffAnswer(answer) = message else {
            return None;
        };
        Some(&answer.answer)
    }

    #[tokio::test]
    async fn a_chats_changes_are_pushed_and_one_without_a_worktree_is_refused() {
        use weacon_protocol::DiffAnswerKind;

        let dir = TempDir::new().unwrap();
        let (worktrees, workspace, chat, worktree) = crate::git::service::with_chat(&dir);
        fs::write(worktree.join("agent.txt"), "mine\n").unwrap();
        let running = start_as(&dir, |daemon| daemon.shared.worktrees = worktrees).unwrap();
        let bytes = worktree_asks(&workspace, &chat);

        let (replies, _stream) = exchange(connect(&running.socket).await, &bytes, 5).await;
        running.stop().await;

        let pushed = replies.iter().find_map(diff_answer);
        let Some(DiffAnswerKind::File { files: 1, file }) = pushed else {
            panic!("no change pushed: {replies:?}");
        };
        assert_eq!(file.path, "agent.txt");
        let refused: Vec<&str> = replies.iter().filter_map(error_code).collect();
        assert_eq!(refused, ["worktree.none"; 3]);
    }

    // A chat's pushed events in brief, and the other replies. The run starts off the engine's
    // loop, so a snapshot can come before it is up.
    fn events_and_answers(replies: &[Message]) -> (Vec<String>, Vec<&Message>) {
        use crate::chats::test_support::brief;

        let (events, answers): (Vec<_>, Vec<_>) = replies
            .iter()
            .partition(|reply| matches!(reply, Message::Chat(_)));
        (events.into_iter().map(brief).collect(), answers)
    }

    #[tokio::test]
    async fn chat_commands_and_queries_reach_the_engine_and_its_events_come_back() {
        use crate::chats::test_support::{allow, bench, prompt, query, soon};
        use weacon_protocol::ChatQueryKind;

        let dir = TempDir::new().unwrap();
        let mut bench = bench();
        let chats = bench.chats.clone();
        let running = start_as(&dir, |daemon| daemon.shared.chats = chats).unwrap();
        let line = |id, message| encode_line(&Envelope { id, message }).unwrap();
        let subscribe = query("c-1", ChatQueryKind::Subscribe { after: 0 });
        let bytes = [
            hello(PROTOCOL),
            line(2, Message::ChatQuery(subscribe)),
            line(3, Message::ChatCommand(prompt("r-2", " "))),
            line(4, Message::ChatCommand(prompt("r-1", "MARKER-PROMPT"))),
            line(5, Message::ChatCommand(allow("r-3", "c-1", "run-1", 9))),
            line(6, Message::ChatQuery(query("c-1", ChatQueryKind::Snapshot))),
        ]
        .concat();

        let (replies, _stream) = exchange(connect(&running.socket).await, &bytes, 8).await;
        let records = running.stop().await;

        let Message::Welcome(welcome) = &replies[0] else {
            panic!("no welcome");
        };
        assert_eq!(welcome.capabilities, crate::session::CAPABILITIES);
        let (events, answers) = events_and_answers(&replies[1..]);
        assert_eq!(error_code(answers[0]), Some("chat.empty_prompt"));
        assert_eq!(error_code(answers[1]), Some("agent.no_such_prompt"));
        assert!(matches!(answers[2], Message::ChatAnswer(answer)
            if matches!(answer.answer, weacon_protocol::ChatAnswerKind::Snapshot { seq, .. } if seq >= 2)));
        assert_eq!(
            events,
            [
                "1 prompt r-1 MARKER-PROMPT",
                "2 receipt r-1 accepted",
                "3 run-1 is Starting",
                "4 run-1 is Running",
            ]
        );
        assert!(soon(bench.started.recv()).await.is_some());
        let logged = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(logged.contains("chat command taken"), "{logged}");
        assert!(logged.contains("chat request refused"), "{logged}");
        assert!(logged.contains("r-1"), "{logged}");
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_local_client_holds_a_chat_until_it_hangs_up() {
        use crate::chats::test_support::{bench, command, prompt, query, soon};
        use weacon_protocol::{ChatAction, ChatAnswerKind, ChatQueryKind};

        let dir = TempDir::new().unwrap();
        let mut bench = bench();
        let chats = bench.chats.clone();
        let running = start_as(&dir, |daemon| daemon.shared.chats = chats).unwrap();
        let line = |id, message| encode_line(&Envelope { id, message }).unwrap();
        let hold = || Message::ChatCommand(command("h-1", "c-1", ChatAction::Hold));
        let snapshot = || Message::ChatQuery(query("c-1", ChatQueryKind::Snapshot));
        let asked = [hello(PROTOCOL), line(2, hold()), line(3, snapshot())].concat();
        let (answers, holder) = exchange(connect(&running.socket).await, &asked, 2).await;
        let other = [
            hello(PROTOCOL),
            line(2, Message::ChatCommand(prompt("r-1", "Hi"))),
            line(3, hold()),
            line(4, snapshot()),
        ]
        .concat();
        let (refused, _other) = exchange(connect(&running.socket).await, &other, 3).await;

        assert!(matches!(&answers[1], Message::ChatAnswer(_)), "{answers:?}");
        assert_eq!(error_code(&refused[1]), Some("chat.held"));
        assert!(matches!(&refused[2], Message::ChatAnswer(answer)
            if matches!(answer.answer, ChatAnswerKind::Snapshot { seq: 2, .. })));
        drop(holder);
        assert!(soon(bench.started.recv()).await.is_some());
        running.stop().await;
    }

    #[tokio::test]
    async fn the_owner_lists_their_outside_sessions_through_the_socket_and_relayed_asks_dont() {
        use weacon_protocol::{AgentKind, SessionsAnswerKind, SessionsQuery, SessionsQueryKind};

        let dir = TempDir::new().unwrap();
        let home = test_support::claude_home(&dir, "/work", "Hi");
        let state = dir.join("state");
        let outside = Outside::new(vec![(AgentKind::Claude, home)], Some(&state));
        let running = start_as(&state, |daemon| daemon.shared.outside = outside).unwrap();
        let query = |id, by: Option<&str>| {
            let by = by.map(|account| account.to_owned().try_into().unwrap());
            let query = SessionsQuery {
                request: "r-1".to_owned().try_into().unwrap(),
                machine: "mc-1".to_owned().try_into().unwrap(),
                query: SessionsQueryKind::List,
                by,
            };
            let message = Message::SessionsQuery(query);
            encode_line(&Envelope { id, message }).unwrap()
        };

        let bytes = [hello(PROTOCOL), query(2, None), query(3, Some("a-1"))].concat();
        let (replies, _stream) = exchange(connect(&running.socket).await, &bytes, 3).await;
        running.stop().await;

        assert!(matches!(&replies[1], Message::SessionsAnswer(answer)
            if matches!(&answer.answer, SessionsAnswerKind::List { sessions } if sessions.len() == 1)));
        assert!(error_code(&replies[2]).is_some(), "{:?}", replies[2]);
    }

    #[tokio::test]
    async fn chat_and_sessions_messages_before_the_hello_are_refused_like_any_other() {
        use crate::chats::test_support::{bench, prompt, query};
        use weacon_protocol::ChatQueryKind;

        let dir = TempDir::new().unwrap();
        let mut bench = bench();
        let chats = bench.chats.clone();
        let running = start_as(&dir, |daemon| daemon.shared.chats = chats).unwrap();
        let line = |id, message| encode_line(&Envelope { id, message }).unwrap();

        for first in [
            Message::ChatCommand(prompt("r-1", "Hi")),
            Message::ChatQuery(query("c-1", ChatQueryKind::Snapshot)),
            Message::SessionsQuery(weacon_protocol::SessionsQuery {
                request: "r-1".to_owned().try_into().unwrap(),
                machine: "mc-1".to_owned().try_into().unwrap(),
                query: weacon_protocol::SessionsQueryKind::List,
                by: None,
            }),
        ] {
            let stream = connect(&running.socket).await;
            let (replies, _stream) = exchange(stream, &line(1, first), 1).await;
            assert_eq!(error_code(&replies[0]), Some("handshake.required"));
        }
        running.stop().await;
        bench.started.try_recv().unwrap_err();
    }

    #[tokio::test]
    async fn the_engine_stops_before_the_daemon_lets_go_of_its_lock() {
        let dir = TempDir::new().unwrap();
        let records = start(&dir).unwrap().stop().await;

        let logged = messages(&records);
        let at = |message| logged.iter().position(|logged| *logged == message);
        assert!(at("engine stopped").unwrap() < at("daemon stopped").unwrap());
    }
}
