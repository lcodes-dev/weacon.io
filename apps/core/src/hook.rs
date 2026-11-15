//! What agents' hooks print. A hook that can't answer in time prints nothing and succeeds, so
//! it never stops an agent.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use weacon_agents::TOKEN_VARIABLE;
use weacon_appkit::{Ending, build};
use weacon_client::Connection;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    App as Build, Capability, Envelope, MapArgs, MapQuery, MapQueryKind, McpCall, Message, Secret,
    code,
};

use crate::app::{App, runtime};
use crate::cli::HookCommand;
use crate::daemon::socket_path;
use crate::map;

/// The most a session-start hook adds to an agent's context.
const SESSION_START: MapArgs = MapArgs { budget: Some(4000) };
const LIMIT: Duration = Duration::from_secs(5);
const MAP_READ: Capability = Capability::new(code!("map.read"));

/// How a hook finds its folder's map: from its run's core when it has one, else by `index`,
/// within `limit` in all.
struct Hook {
    core: Option<Core>,
    index: fn(&McpCall, &Path) -> Result<String, Fault>,
    limit: Duration,
}

/// The core that started the hook's run, and that run's token for its MCP.
struct Core {
    socket: PathBuf,
    token: Secret,
    me: Build,
}

/// Prints what `command`'s hook adds for the current folder, or nothing.
pub(crate) fn run(app: &App, command: HookCommand, out: &mut impl Write) -> Ending {
    let token = std::env::var(TOKEN_VARIABLE).ok();
    let core = app
        .state_dir()
        .ok()
        .zip(token)
        .map(|(state_dir, token)| Core {
            socket: socket_path(state_dir),
            token: token.into(),
            me: build(&app.version),
        });
    let hook = Hook {
        core,
        index: map::map_or_empty,
        limit: LIMIT,
    };
    run_in(app, command, &hook, std::env::current_dir(), out)
}

/// Prints what `command`'s hook adds for `folder`, or nothing when it can't be found.
fn run_in(
    app: &App,
    command: HookCommand,
    hook: &Hook,
    folder: io::Result<PathBuf>,
    out: &mut impl Write,
) -> Ending {
    let HookCommand::SessionStart = command;
    let printed = folder
        .map_err(|error| skipped().because(error))
        .and_then(|folder| hook.session_start(folder, &app.log))
        .and_then(|text| {
            out.write_all(text.as_bytes())
                .map_err(|error| skipped().because(error))
        });
    note(&app.log, printed);
    Ending::Done
}

impl Hook {
    /// Where to begin in `folder`'s code, or nothing when it has no code map.
    fn session_start(&self, folder: PathBuf, log: &Logger) -> Result<String, Fault> {
        let call = McpCall::Map(SESSION_START);
        let index = self.index;
        within(self.limit, async move {
            if let Some(text) = self.stored(&folder, log).await {
                return Ok(text);
            }
            let indexed = tokio::task::spawn_blocking(move || index(&call, &folder));
            indexed.await.map_err(|error| skipped().because(error))?
        })
    }

    /// The core's text for `folder`, from its stored map; none when there is no core, or it has
    /// no map of the folder.
    async fn stored(&self, folder: &Path, log: &Logger) -> Option<String> {
        let core = self.core.as_ref()?;
        core.stored(folder.to_str()?)
            .await
            .inspect_err(|fault| log.info("hook core skipped", &[("err", Field::error(fault))]))
            .ok()
            .flatten()
    }
}

impl Core {
    async fn stored(&self, folder: &str) -> Result<Option<String>, Fault> {
        let (welcome, mut connection) =
            Connection::open(&self.socket, self.me.clone(), Vec::new()).await?;
        if !welcome.capabilities.contains(&MAP_READ) {
            return Ok(None);
        }
        let query = MapQuery {
            token: self.token.clone(),
            folder: folder.to_owned(),
            query: MapQueryKind::Map(SESSION_START),
        };
        let message = Message::MapQuery(query);
        connection.send(&Envelope { id: 2, message }).await?;
        loop {
            match connection.next().await? {
                Some(Ok(Message::MapAnswer(answer))) => return Ok(answer.text),
                Some(Ok(Message::Error(_))) | None => return Ok(None),
                Some(_) => {}
            }
        }
    }
}

/// `work`'s answer, or a fault once `limit` passes.
fn within<T>(limit: Duration, work: impl Future<Output = Result<T, Fault>>) -> Result<T, Fault> {
    let runtime = runtime()?;
    let answer = runtime.block_on(async { tokio::time::timeout(limit, work).await });
    // Work still reading a large folder is left to the exit rather than waited for.
    runtime.shutdown_background();
    answer.map_err(|_| Fault::new(code!("hook.too_slow"), "The hook couldn't answer in time"))?
}

// Only at info, which a hook's stderr never shows, so a hook that can't answer prints nothing.
fn note(log: &Logger, printed: Result<(), Fault>) {
    if let Err(fault) = printed {
        log.info("hook skipped", &[("err", Field::error(&fault))]);
    }
}

fn skipped() -> Fault {
    Fault::new(code!("hook.failed"), "The hook couldn't answer")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use weacon_agents::SpawnPolicy;
    use weacon_protocol::{RepoId, RunId, WorkspaceId};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::chats::Places;
    use crate::daemon::Daemon;
    use crate::git::state::Stores;
    use crate::git::{fixture, git, repo};
    use crate::mcp::grants::{Grant, Revoke};
    use crate::mcp::test_support::workspace;
    use crate::test_support::{VERSION, app, messages};
    use crate::workspaces::Workspaces;

    /// Long enough that no test meets it.
    const NEVER: Duration = Duration::from_secs(3600);

    fn repository(dir: &Path, item: &str) {
        fs::create_dir_all(dir.join("src")).unwrap();
        let manifest = format!("[package]\nname = \"{item}\"\nversion = \"0.1.0\"\n");
        fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        fs::write(dir.join("src/lib.rs"), format!("pub fn {item}() {{}}\n")).unwrap();
    }

    // Large enough that its map is cut to the size a session start may take.
    fn crates(dir: &Path) {
        for n in 0..120 {
            repository(&dir.join(format!("crate_number_{n}")), &format!("item_{n}"));
        }
    }

    fn indexed(_: &McpCall, _: &Path) -> Result<String, Fault> {
        Ok("indexed\n".to_owned())
    }

    fn fallback() -> Hook {
        Hook {
            core: None,
            index: map::map_or_empty,
            limit: NEVER,
        }
    }

    fn asking(socket: PathBuf, token: &str) -> Hook {
        let core = Core {
            socket,
            token: token.into(),
            me: build(&VERSION),
        };
        Hook {
            core: Some(core),
            index: indexed,
            limit: NEVER,
        }
    }

    fn sized(text: &str) {
        assert!((3000..=4000).contains(&text.len()), "{}", text.len());
        assert!(text.ends_with('\n'));
    }

    /// A core serving `state` on its own thread, so a hook can block on its own runtime.
    struct Running {
        socket: PathBuf,
        mcp: crate::mcp::Mcp,
        stop: tokio::sync::oneshot::Sender<()>,
        served: std::thread::JoinHandle<()>,
    }

    impl Running {
        fn start(state: &Path) -> Self {
            let places = Places {
                path: std::env::var_os("PATH"),
                ..Places::default()
            };
            let (started, mcp) = std::sync::mpsc::channel();
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let dir = state.to_owned();
            let served = std::thread::spawn(move || {
                runtime().unwrap().block_on(async move {
                    let (log, _) = recording_logger();
                    let daemon =
                        Daemon::start(&dir, build(&VERSION), SpawnPolicy::default(), places, log);
                    let daemon = daemon.unwrap();
                    started.send(daemon.mcp().clone()).unwrap();
                    daemon.serve(async { drop(stopped.await) }).await;
                });
            });
            Self {
                socket: socket_path(state),
                mcp: mcp.recv().unwrap(),
                stop,
                served,
            }
        }

        fn stop(self) {
            self.stop.send(()).unwrap();
            self.served.join().unwrap();
        }

        fn token(&self, workspace: &WorkspaceId, folder: &Path) -> (String, Revoke) {
            let run = RunId::try_from("run-1".to_owned()).unwrap();
            let grant = Grant::new(workspace.clone(), run, folder.to_owned());
            let (token, revoke) = self.mcp.grants.issue(grant).unwrap();
            (token.as_str().to_owned(), revoke)
        }
    }

    // Registers `repo` for `workspace` in `dir/state`, made private as the core requires.
    fn register(dir: &Path, workspace: &WorkspaceId, repo: &Path) -> PathBuf {
        let state = dir.join("state");
        crate::platform::private_dir(&state).unwrap();
        let repos = Stores::new(&Workspaces::new(&state, None), workspace).repos();
        let id = RepoId::try_from("site".to_owned()).unwrap();
        repos.add(&git(dir), id, repo).unwrap();
        state
    }

    #[test]
    fn a_running_core_answers_from_its_stored_map_without_indexing() {
        let dir = TempDir::new().unwrap();
        let one = workspace("one");
        let main = repo(&dir);
        crates(&main);
        fixture(&dir, &main, &["add", "."]);
        fixture(&dir, &main, &["commit", "--quiet", "-m", "Add crates"]);
        let main = main.canonicalize().unwrap();
        let core = Running::start(&register(&dir, &one, &main));
        let (token, revoke) = core.token(&one, &main);
        let (log, _) = recording_logger();
        let start = |hook: &Hook, folder: &Path| hook.session_start(folder.to_owned(), &log);

        let text = start(
            &asking(core.socket.clone(), &token),
            &main.join("crate_number_3"),
        );
        sized(&text.unwrap());
        let outside = dir.join("outside");
        fs::create_dir(&outside).unwrap();
        let outside = start(&asking(core.socket.clone(), &token), &outside);
        assert_eq!(outside.unwrap(), "indexed\n");
        let stranger = start(&asking(core.socket.clone(), "other"), &main);
        assert_eq!(stranger.unwrap(), "indexed\n");
        drop(revoke);
        let ended = start(&asking(core.socket.clone(), &token), &main);
        assert_eq!(ended.unwrap(), "indexed\n");
        core.stop();
    }

    #[test]
    fn a_registered_repository_without_code_adds_nothing_from_the_core() {
        let dir = TempDir::new().unwrap();
        let one = workspace("one");
        let main = repo(&dir).canonicalize().unwrap();
        let core = Running::start(&register(&dir, &one, &main));
        let (token, _revoke) = core.token(&one, &main);
        let (log, _) = recording_logger();

        let text = asking(core.socket.clone(), &token).session_start(main, &log);

        assert_eq!(text.unwrap(), "");
        core.stop();
    }

    #[test]
    fn without_a_core_the_hook_indexes_its_folder_within_a_fixed_size() {
        let dir = TempDir::new().unwrap();
        crates(&dir);
        let (log, records) = recording_logger();

        let text = fallback().session_start(dir.to_path_buf(), &log);
        let unanswered =
            asking(dir.join("weacon.sock"), "t").session_start(dir.to_path_buf(), &log);

        sized(&text.unwrap());
        assert_eq!(unanswered.unwrap(), "indexed\n");
        assert_eq!(messages(&records), ["hook core skipped"]);
    }

    #[test]
    fn a_folder_without_code_adds_nothing_and_a_missing_one_fails() {
        let dir = TempDir::new().unwrap();
        let (log, _) = recording_logger();
        let start = |folder: PathBuf| fallback().session_start(folder, &log);

        assert_eq!(start(dir.to_path_buf()).unwrap(), "");
        let missing = start(dir.join("gone")).unwrap_err();
        assert_eq!(missing.code().as_str(), "map.read_failed");
    }

    #[test]
    fn work_that_takes_too_long_is_left_behind() {
        let (release, wait) = std::sync::mpsc::channel::<()>();

        let late = within(Duration::from_millis(10), async move {
            let waiting = tokio::task::spawn_blocking(move || wait.recv());
            waiting.await.map_err(|_| skipped())?.map_err(|_| skipped())
        });

        assert_eq!(late.unwrap_err().code().as_str(), "hook.too_slow");
        drop(release);
        assert_eq!(within(NEVER, async { Ok(1) }).unwrap(), 1);
    }

    #[test]
    fn the_hook_prints_its_folders_map_and_always_succeeds() {
        let dir = TempDir::new().unwrap();
        repository(&dir.join("a"), "marker_item_1");
        let app = app(None, None);
        let printed = |folder: io::Result<PathBuf>| {
            let mut out = Vec::new();
            let ending = run_in(
                &app,
                HookCommand::SessionStart,
                &fallback(),
                folder,
                &mut out,
            );
            (ending, String::from_utf8(out).unwrap())
        };

        let (ending, map) = printed(Ok(dir.to_path_buf()));
        assert_eq!(ending, Ending::Done);
        assert!(map.contains("marker_item_1") && map.len() <= 4000, "{map}");
        let empty = TempDir::new().unwrap();
        assert_eq!(
            printed(Ok(empty.to_path_buf())),
            (Ending::Done, String::new())
        );
        let unknown = Err(io::ErrorKind::NotFound.into());
        assert_eq!(printed(unknown), (Ending::Done, String::new()));
    }

    #[test]
    fn neither_the_code_nor_its_path_nor_the_token_reaches_the_log() {
        let dir = TempDir::new().unwrap();
        let folder = dir.join("marker_folder_3");
        repository(&folder, "marker_item_7");
        let (log, records) = recording_logger();
        let hook = Hook {
            index: map::map_or_empty,
            ..asking(dir.join("marker_socket_9"), "marker_token_2")
        };

        let map = hook.session_start(folder.clone(), &log);
        note(
            &log,
            hook.session_start(folder.join("marker_gone_5"), &log)
                .map(drop),
        );

        assert!(map.unwrap().contains("marker_item_7"));
        let skips = ["hook core skipped", "hook core skipped", "hook skipped"];
        assert_eq!(messages(&records), skips);
        let logged = format!("{:?}", records.lock().unwrap());
        assert!(!logged.contains("marker"), "{logged}");
    }
}
