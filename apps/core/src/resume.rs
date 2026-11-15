//! `weacon resume`: a chat's agent, carried on by hand in the person's terminal, on the session
//! the chat holds, in the chat's folder, with the workspace's own sign-in.

use std::future::Future;
use std::path::Path;

use weacon_client::Connection;
use weacon_observability::{Fault, Logger};
use weacon_protocol::{
    AgentKind, AgentSession, Capability, ChatAction, ChatCommand, ChatId, ChatQuery, ChatQueryKind,
    Code, Envelope, Message, RequestId, RunState, WorkspaceId, code,
};

use crate::app::{App, runtime};
use crate::chats;
use crate::chats::log::ChatLog;
use crate::daemon::{build, socket_path};
use crate::runs::{ChatKey, Runs};
use crate::workspaces::Workspaces;

const HOLD: u32 = 2;
const BARRIER: u32 = 3;
const RELEASE: u32 = 4;
/// How long the daemon may take to answer whether it holds the chat.
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs the chat's agent here until the person ends it.
pub(crate) fn run(app: &App, workspace: &WorkspaceId, chat: &ChatId) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    let key = ChatKey::new(workspace, chat);
    let (agent, _) = asleep(state_dir, &key)?;
    let workspaces = Workspaces::new(state_dir, app.path.clone());
    let root = workspaces.root(workspace);
    let rt = runtime()?;
    let repo = rt.block_on(ChatLog::open(&root, chat))?.repo().cloned();
    let cwd = chats::chat_dir(&workspaces, app.work_root.as_deref(), &key, repo.as_ref())?;
    let launch = workspaces.launch(workspace, agent, cwd)?;
    let log = app.log.child(&[
        ("workspace_id", workspace.as_str().into()),
        ("chat_id", chat.as_str().into()),
    ]);
    let (dir, again) = (state_dir.to_owned(), key.clone());
    let agent = async {
        // Read again under the hold: until it was taken, the daemon could have started the agent.
        let (_, session) = crate::app::blocking(move || Ok(asleep(&dir, &again)))
            .await
            .map_err(|error| {
                Fault::new(code!("runs.unreadable"), "The runs journal can't be read")
                    .because(error)
            })??;
        launch.require_sign_in().await?;
        log.info("resume started", &[]);
        launch.resume_by_hand(&session).await
    };
    rt.block_on(async {
        let held = hold(&socket_path(state_dir), build(&app.version), &key).await?;
        carry_on(held, agent, &log).await
    })?;
    log.info("resume ended", &[]);
    Ok(())
}

// The chat's agent and session, as the runs journal has them, while no agent runs it here.
fn asleep(state_dir: &Path, key: &ChatKey) -> Result<(AgentKind, AgentSession), Fault> {
    let runs = Runs::load(state_dir)?;
    // A newer core ran it last, and carries it on.
    runs.refuse_newer(key)?;
    let record = runs.of(key).ok_or_else(|| {
        Fault::new(
            code!("resume.no_chat"),
            "This chat hasn't run on this machine",
        )
    })?;
    let session = record.session.clone().ok_or_else(|| {
        Fault::new(
            code!("resume.no_session"),
            "This chat has no session to carry on yet",
        )
    })?;
    // Two agents on one session would both write it.
    if !matches!(record.state, RunState::Sleeping | RunState::Ended) {
        return Err(Fault::new(
            code!("resume.agent_running"),
            "The chat's agent is still running here; carry on once it sleeps",
        ));
    }
    Ok((record.agent, session))
}

/// The daemon's hold on a chat, for as long as its connection is open.
struct Held {
    daemon: Connection,
    release: Message,
}

impl Held {
    // A release that doesn't arrive is no matter: the closed connection releases the chat too.
    async fn release(mut self) {
        let release = Envelope {
            id: RELEASE,
            message: self.release,
        };
        drop(self.daemon.send(&release).await);
    }
}

// Holds the chat in the daemon, so it starts no agent on the same session; `None` with no
// daemon, or one that can't hold a chat, since nothing could start one then.
async fn hold(
    socket: &Path,
    me: weacon_protocol::App,
    key: &ChatKey,
) -> Result<Option<Held>, Fault> {
    let (welcome, daemon) = match Connection::open(socket, me, Vec::new()).await {
        Ok(opened) => opened,
        Err(fault) if fault.code() == &code!("daemon.not_running") => return Ok(None),
        Err(fault) => return Err(fault),
    };
    if !welcome
        .capabilities
        .contains(&Capability::new(code!("chat.hold")))
    {
        return Ok(None);
    }
    take(daemon, key).await.map(Some)
}

async fn take(mut daemon: Connection, key: &ChatKey) -> Result<Held, Fault> {
    let message = chat_command(key, ChatAction::Hold)?;
    daemon.send(&Envelope { id: HOLD, message }).await?;
    // The daemon answers a connection in order: a refusal of the hold comes before this answer.
    let barrier = Message::ChatQuery(ChatQuery {
        workspace: key.workspace.clone(),
        chat: key.chat.clone(),
        query: ChatQueryKind::Window {
            before: 1,
            limit: 0,
        },
    });
    let barrier = Envelope {
        id: BARRIER,
        message: barrier,
    };
    daemon.send(&barrier).await?;
    // Unanswered, the connection goes, and any hold taken with it.
    let answered = tokio::time::timeout(ANSWER_WITHIN, answer(&mut daemon)).await;
    answered.map_err(|_| no_answer())??;
    let release = chat_command(key, ChatAction::Release)?;
    Ok(Held { daemon, release })
}

// Waits for the barrier's answer, or the hold's refusal before it.
async fn answer(daemon: &mut Connection) -> Result<(), Fault> {
    while let Some(frame) = daemon.next().await? {
        match frame {
            Ok(Envelope {
                id: HOLD,
                message: Message::Error(error),
            }) => return Err(refused(error.code)),
            Ok(Envelope { id: BARRIER, .. }) => return Ok(()),
            _ => {}
        }
    }
    Err(no_answer())
}

// Why the daemon wouldn't hold the chat, in words the person can act on.
fn refused(code: Code) -> Fault {
    let why = match code.as_str() {
        "chat.busy" => {
            "The chat's agent is running here, or an update is installing; carry on once it's done"
        }
        "chat.held" => "Someone else carries this chat on by hand already",
        _ => "The daemon wouldn't hold the chat",
    };
    Fault::new(code, why)
}

fn no_answer() -> Fault {
    Fault::new(code!("daemon.no_answer"), "The daemon didn't answer")
}

// Runs `agent`, then releases the chat. Should the daemon go first, as when it restarts, the
// hold is gone: the person is warned, and the agent goes on.
async fn carry_on(
    held: Option<Held>,
    agent: impl Future<Output = Result<(), Fault>>,
    log: &Logger,
) -> Result<(), Fault> {
    let Some(mut held) = held else {
        return agent.await;
    };
    let mut agent = std::pin::pin!(agent);
    tokio::select! {
        biased;
        () = hung_up(&mut held.daemon) => {
            log.warn("chat hold lost", &[]);
            agent.await
        }
        ran = &mut agent => {
            held.release().await;
            ran
        }
    }
}

// Waits for the daemon to hang up; nothing else comes on this connection.
async fn hung_up(daemon: &mut Connection) {
    while let Ok(Some(_)) = daemon.next().await {}
}

// `action` for the chat. The engine never logs a hold or a release, nor checks their ids.
fn chat_command(key: &ChatKey, action: ChatAction) -> Result<Message, Fault> {
    let request = RequestId::try_from("hold".to_owned())
        .map_err(|_| Fault::new(code!("request.invalid"), "The request id isn't valid"))?;
    Ok(Message::ChatCommand(ChatCommand {
        request,
        workspace: key.workspace.clone(),
        chat: key.chat.clone(),
        action,
        by: None,
    }))
}

#[cfg(test)]
mod tests {

    use std::task::Poll;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;
    use weacon_protocol::{
        AgentKind, AgentSession, ChatAnswer, ChatAnswerKind, Code, ErrorBody, Welcome, encode_line,
    };
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, VERSION, messages, recording_logger};

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn ids() -> (WorkspaceId, ChatId) {
        (
            WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            ChatId::try_from("c-1".to_owned()).unwrap(),
        )
    }

    // The chat's record in the journal, as its last run left it.
    fn chat_left(dir: &Path, state: RunState, session: Option<&str>) {
        let (workspace, chat) = ids();
        let key = ChatKey::new(&workspace, &chat);
        runtime().unwrap().block_on(async {
            let mut runs = Runs::load(dir).unwrap();
            runs.begin(&key, AgentKind::Claude, 100).await.unwrap();
            let session = session.map(|id| AgentSession::try_from(id.to_owned()).unwrap());
            runs.update(&key, |record| {
                record.state = state;
                record.session = session;
            })
            .await
            .unwrap();
        });
    }

    fn resume(dir: &TempDir) -> Result<(), Fault> {
        let mut app = test_support::app(Some(&dir.join("state")), None);
        app.work_root = Some(dir.join("work"));
        app.path = Some(dir.join("bin").into_os_string());
        let (workspace, chat) = ids();
        run(&app, &workspace, &chat)
    }

    fn code_of(result: Result<(), Fault>) -> String {
        result.unwrap_err().code().as_str().to_owned()
    }

    #[test]
    fn a_chat_with_no_session_here_has_nothing_to_carry_on() {
        let dir = TempDir::new().unwrap();
        assert_eq!(code_of(resume(&dir)), "resume.no_chat");
        chat_left(&dir.join("state"), RunState::Sleeping, None);
        assert_eq!(code_of(resume(&dir)), "resume.no_session");
    }

    #[test]
    fn a_chat_a_newer_core_ran_is_left_to_it() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let newer = r#"{"workspace":"w-1","chat":"c-1","agent":"gemini","state":"sleeping"}"#;
        let journal = format!(r#"{{"issued":1,"chats":[{newer}]}}"#);
        std::fs::write(state.join("runs.json"), journal).unwrap();

        assert_eq!(code_of(resume(&dir)), "runs.newer_record");
    }

    #[test]
    fn a_chat_whose_agent_is_alive_isnt_carried_on_beside_it() {
        for state in [
            RunState::Starting,
            RunState::Running,
            RunState::Waiting,
            RunState::Idle,
        ] {
            let dir = TempDir::new().unwrap();
            chat_left(&dir.join("state"), state, Some(SESSION));
            assert_eq!(code_of(resume(&dir)), "resume.agent_running", "{state:?}");
        }
    }

    #[test]
    fn a_chat_in_a_repository_is_carried_on_only_in_its_worktree() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        chat_left(&state, RunState::Sleeping, Some(SESSION));
        let (workspace, chat) = ids();
        let root = Workspaces::new(&state, None).root(&workspace);
        let first = weacon_protocol::ChatEvent::Prompt {
            request: "r-1".parse().unwrap(),
            text: "Go".to_owned(),
            repo: Some("site".parse().unwrap()),
            agent: None,
        };
        runtime().unwrap().block_on(async {
            let mut log = ChatLog::open(&root, &chat).await.unwrap();
            log.append(first).await.unwrap();
        });

        // Its repository is gone from the registry, so it has nowhere to carry on.
        assert_eq!(code_of(resume(&dir)), "repo.unknown");
        assert!(!dir.join("work").exists());
    }

    // A stand-in for Claude Code: signed in from its config folder, or not.
    #[cfg(unix)]
    const SIGNED_IN: &str =
        "[ \"$1\" = auth ] && { printf '{\"authMethod\":\"claude.ai\"}'; exit 0; }";
    #[cfg(unix)]
    const RECORDS: &str = "printf '%s %s %s' \"$*\" \"$PWD\" \"$CLAUDE_CONFIG_DIR\" > \"$0.ran\"\n";

    #[cfg(unix)]
    #[test]
    fn a_chat_whose_agent_isnt_signed_in_from_its_workspace_isnt_carried_on() {
        use weacon_testing::platform::write_program;

        let dir = TempDir::new().unwrap();
        chat_left(&dir.join("state"), RunState::Sleeping, Some(SESSION));
        let script = format!("#!/bin/sh\n[ \"$1\" = auth ] && exit 1\n{RECORDS}");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        write_program(&dir.join("bin/claude"), &script).unwrap();

        assert_eq!(code_of(resume(&dir)), "agent.signed_out");
        assert!(!dir.join("bin/claude.ran").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_sleeping_chat_is_carried_on_in_its_folder_with_the_workspaces_sign_in() {
        use std::fs;

        use weacon_testing::platform::write_program;

        for state in [RunState::Sleeping, RunState::Ended] {
            let dir = TempDir::new().unwrap();
            chat_left(&dir.join("state"), state, Some(SESSION));
            let script = format!("#!/bin/sh\n{SIGNED_IN}\n{RECORDS}");
            fs::create_dir_all(dir.join("bin")).unwrap();
            write_program(&dir.join("bin/claude"), &script).unwrap();
            resume(&dir).unwrap();
            let ran = fs::read_to_string(dir.join("bin/claude.ran")).unwrap();
            let (workspace, chat) = ids();
            let workspace = crate::workspaces::folder_name(&workspace);
            // The shell's $PWD has no links in it: macOS's temporary folder is one.
            let folder = fs::canonicalize(
                dir.join("work")
                    .join(&workspace)
                    .join(chats::file_stem(&chat)),
            )
            .unwrap();
            let config = dir.join("state/workspaces").join(workspace).join("claude");
            let expected = format!(
                "--setting-sources user --resume {SESSION} {} {}",
                folder.display(),
                config.display()
            );
            assert_eq!(ran, expected, "{state:?}");
        }
    }

    /// What the fake daemon does.
    #[derive(Default)]
    struct Fake {
        offered: Vec<Capability>,
        refusal: Option<Code>,
        /// Run as the hold arrives, before it is answered.
        on_hold: Option<Box<dyn FnOnce() + Send>>,
        /// Hangs up once the barrier is answered.
        hangs_up: bool,
        /// Never answers the barrier, and stays connected.
        silent: bool,
    }

    fn holding() -> Fake {
        Fake {
            offered: vec![Capability::new(code!("chat.hold"))],
            ..Fake::default()
        }
    }

    // A daemon at `socket` that passes on each message it is sent.
    fn daemon(socket: &Path, mut fake: Fake) -> mpsc::UnboundedReceiver<Message> {
        let mut listener = crate::platform::bind(socket).unwrap();
        let (sent, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            answer(stream, &mut fake, &sent).await;
        });
        received
    }

    async fn answer(
        stream: crate::platform::Stream,
        fake: &mut Fake,
        sent: &mpsc::UnboundedSender<Message>,
    ) {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let Envelope { id, message } = serde_json::from_str(&line).unwrap();
            let answer = reply(&message, fake);
            let barrier = matches!(message, Message::ChatQuery(_));
            // Passed on before it is answered, so the client never runs ahead of it.
            sent.send(message).unwrap();
            if let Some(message) = answer {
                let line = encode_line(&Envelope { id, message }).unwrap();
                writer.write_all(&line).await.unwrap();
            }
            if barrier && fake.hangs_up {
                return;
            }
        }
    }

    fn reply(message: &Message, fake: &mut Fake) -> Option<Message> {
        if matches!(message, Message::Hello(_)) {
            return Some(Message::Welcome(Welcome {
                app: build(&VERSION),
                capabilities: fake.offered.clone(),
            }));
        }
        if let Message::ChatQuery(query) = message {
            if fake.silent {
                return None;
            }
            return Some(Message::ChatAnswer(ChatAnswer {
                workspace: query.workspace.clone(),
                chat: query.chat.clone(),
                answer: ChatAnswerKind::Window { events: Vec::new() },
            }));
        }
        let hold =
            matches!(message, Message::ChatCommand(command) if command.action == ChatAction::Hold);
        if let Some(on_hold) = fake.on_hold.take_if(|_| hold) {
            on_hold();
        }
        let code = fake.refusal.clone().filter(|_| hold)?;
        Some(Message::Error(ErrorBody {
            code,
            retryable: false,
        }))
    }

    fn key() -> ChatKey {
        let (workspace, chat) = ids();
        ChatKey::new(&workspace, &chat)
    }

    // What the daemon was sent, in brief, as far as it has read.
    fn sent(received: &mut mpsc::UnboundedReceiver<Message>) -> Vec<String> {
        std::iter::from_fn(|| received.try_recv().ok())
            .map(|message| {
                if let Message::ChatCommand(command) = &message {
                    return format!("{:?}", command.action);
                }
                if let Message::ChatQuery(query) = &message {
                    return format!("{:?}", query.query);
                }
                let hello = matches!(message, Message::Hello(_));
                if hello { "hello" } else { "another" }.to_owned()
            })
            .collect()
    }

    async fn all_sent(mut received: mpsc::UnboundedReceiver<Message>) -> Vec<Message> {
        let mut all = Vec::new();
        while let Some(message) = bounded(received.recv()).await {
            all.push(message);
        }
        all
    }

    async fn held(socket: &Path) -> Result<Option<Held>, Fault> {
        bounded(hold(socket, build(&VERSION), &key())).await
    }

    // What `future` gives, failing the test should it take more than a few seconds.
    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        let future = Box::pin(future);
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_chat_is_held_while_its_agent_runs_and_released_after() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let mut received = daemon(&socket, holding());
        let barrier = "Window { before: 1, limit: 0 }";

        let agent = async {
            assert_eq!(sent(&mut received), ["hello", "Hold", barrier]);
            Ok(())
        };
        let taken = held(&socket).await.unwrap();
        bounded(carry_on(taken, agent, &recording_logger().0))
            .await
            .unwrap();

        let release = bounded(received.recv()).await;
        assert!(matches!(release, Some(Message::ChatCommand(command))
            if command.action == ChatAction::Release && command.chat == key().chat));
        assert!(
            bounded(received.recv()).await.is_none(),
            "the connection stayed open"
        );
    }

    #[tokio::test]
    async fn a_refused_hold_stops_with_its_code() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let refusal = Some(code!("chat.busy"));
        let _received = daemon(
            &socket,
            Fake {
                refusal,
                ..holding()
            },
        );

        let refused = held(&socket).await.map(|_| ());

        assert_eq!(code_of(refused), "chat.busy");
    }

    #[tokio::test]
    async fn a_hold_refused_for_a_running_agent_or_another_holder_says_which() {
        let said = [
            (
                "chat.busy",
                "The chat's agent is running here, or an update is installing; carry on once it's done",
            ),
            (
                "chat.held",
                "Someone else carries this chat on by hand already",
            ),
            ("chat.unknown", "The daemon wouldn't hold the chat"),
        ];
        for (code, why) in said {
            let dir = TempDir::new().unwrap();
            let socket = dir.join("weacon.sock");
            let refusal = Some(Code::new(code));
            let _received = daemon(
                &socket,
                Fake {
                    refusal,
                    ..holding()
                },
            );

            let fault = held(&socket).await.map(|_| ()).unwrap_err();

            assert_eq!(
                (fault.code().as_str(), fault.to_string().as_str()),
                (code, why)
            );
        }
    }

    #[tokio::test]
    async fn a_daemon_that_never_answers_whether_it_holds_the_chat_is_let_go() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let silent = Fake {
            silent: true,
            ..holding()
        };
        let mut received = daemon(&socket, silent);

        let waiting = tokio::spawn(async move { hold(&socket, build(&VERSION), &key()).await });
        for _ in ["hello", "Hold", "barrier"] {
            assert!(bounded(received.recv()).await.is_some());
        }
        // Paused only once nothing is in flight on the socket, so the clock can't outrun it.
        tokio::time::pause();
        let answered = tokio::time::timeout(ANSWER_WITHIN * 2, waiting).await;
        let refused = answered.unwrap().unwrap().map(|_| ());
        tokio::time::resume();

        assert_eq!(code_of(refused), "daemon.no_answer");
        // The connection is gone, and the hold with it.
        assert!(all_sent(received).await.is_empty());
    }

    #[tokio::test]
    async fn a_daemon_that_doesnt_answer_stops_resume_with_its_code() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let mut listener = crate::platform::bind(&socket).unwrap();
        tokio::spawn(async move { drop(listener.accept().await.unwrap()) });

        let refused = held(&socket).await.map(|_| ());

        assert_eq!(code_of(refused), "daemon.no_answer");
    }

    #[tokio::test]
    async fn with_no_daemon_or_one_that_cant_hold_there_is_nothing_to_hold() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        assert!(held(&socket).await.unwrap().is_none());

        let mut received = daemon(&socket, Fake::default());
        assert!(held(&socket).await.unwrap().is_none());

        assert_eq!(bounded(received.recv()).await.map(|_| ()), Some(()));
        assert!(
            bounded(received.recv()).await.is_none(),
            "{:?}",
            sent(&mut received)
        );
    }

    #[tokio::test]
    async fn a_daemon_that_hangs_up_meanwhile_is_warned_of_once_and_the_agent_goes_on() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let _received = daemon(
            &socket,
            Fake {
                hangs_up: true,
                ..holding()
            },
        );
        let (log, records) = recording_logger();
        let warned = || {
            let logged = messages(&records);
            logged
                .iter()
                .filter(|logged| **logged == "chat hold lost")
                .count()
        };

        // Polled again once the hang-up is handled, the agent ends only after the warning.
        let agent = std::future::poll_fn(|_| match warned() {
            0 => Poll::Pending,
            _ => Poll::Ready(Ok(())),
        });
        let taken = held(&socket).await.unwrap();
        bounded(carry_on(taken, agent, &log)).await.unwrap();

        assert_eq!(warned(), 1);
    }

    // A fake daemon apart, for a test that runs a shell script as the agent.
    #[cfg(unix)]
    mod apart {
        use std::path::PathBuf;

        use super::*;

        // A fake daemon on its own thread, since `run` makes its own runtime; it gives back all it
        // was sent once the client hangs up.
        fn daemon_apart(socket: PathBuf, fake: Fake) -> std::thread::JoinHandle<Vec<Message>> {
            let (ready, listening) = std::sync::mpsc::channel();
            let apart =
                std::thread::spawn(move || runtime().unwrap().block_on(serve(socket, fake, ready)));
            listening.recv().unwrap();
            apart
        }

        async fn serve(
            socket: PathBuf,
            fake: Fake,
            ready: std::sync::mpsc::Sender<()>,
        ) -> Vec<Message> {
            let received = daemon(&socket, fake);
            ready.send(()).unwrap();
            all_sent(received).await
        }

        #[test]
        fn a_chat_whose_agent_started_before_the_hold_is_released_and_left_to_it() {
            use std::fs;

            use weacon_testing::platform::write_program;

            let dir = TempDir::new().unwrap();
            let state = dir.join("state");
            chat_left(&state, RunState::Sleeping, Some(SESSION));
            let journal = state.join("runs.json");
            let running = fs::read_to_string(&journal)
                .unwrap()
                .replace("\"sleeping\"", "\"running\"");
            fs::create_dir_all(dir.join("bin")).unwrap();
            write_program(&dir.join("bin/claude"), "#!/bin/sh\ntouch \"$0.ran\"\n").unwrap();
            let on_hold = move || fs::write(journal, running).unwrap();
            let fake = Fake {
                on_hold: Some(Box::new(on_hold)),
                ..holding()
            };
            let daemon = daemon_apart(socket_path(&state), fake);

            assert_eq!(code_of(resume(&dir)), "resume.agent_running");
            let sent = daemon.join().unwrap();
            assert!(matches!(sent.last(), Some(Message::ChatCommand(command))
                if command.action == ChatAction::Release));
            assert!(!dir.join("bin/claude.ran").exists());
        }
    }
}
