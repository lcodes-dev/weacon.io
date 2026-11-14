//! Chats: each one's log of committed events, and the engine that runs their agents.

pub(crate) mod engine;
pub(crate) mod feed;
pub(crate) mod log;
#[cfg(test)]
pub(crate) mod test_support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use weacon_agents::SpawnPolicy;
use weacon_observability::{Fault, Logger};
use weacon_protocol::{App, ChatId, WorkspaceId, code};

pub(crate) use engine::Chats;
use engine::{Engine, Host, Start};

use crate::mcp::grants::Granted;
use crate::mcp::{self, Mcp};
use crate::platform;
use crate::runs::Runs;
use crate::workspaces::{self, Workspaces};

/// Where the engine finds things on this machine, beside its state folder.
#[derive(Clone, Debug, Default)]
pub(crate) struct Places {
    /// The `PATH` vendors' programs are found on.
    pub(crate) path: Option<OsString>,
    /// Where chats' working folders go; with none, no agent can start.
    pub(crate) work_root: Option<PathBuf>,
    /// The core's MCP, which runs reach with a token of their own.
    pub(crate) mcp: Option<Mcp>,
}

/// The clients' way to the chats in `state_dir`, and the engine to run behind it, which starts
/// each agent under `policy` with its workspace's own sign-in, in a folder under `places`.
///
/// # Errors
/// When the journal of runs can't be read.
pub(crate) fn start(
    state_dir: &Path,
    places: Places,
    app: App,
    policy: SpawnPolicy,
    log: Logger,
) -> Result<(Chats, Engine<Granted>), Fault> {
    let runs = Runs::load(state_dir)?;
    let workspaces = Workspaces::new(state_dir, places.path.clone());
    let roots = workspaces.clone();
    let host = Host {
        start: Arc::new(move |start: &Start| launch(&roots, &places, &policy, &app, start)),
        now: unix_now,
    };
    Ok(Engine::new(runs, workspaces, host, log))
}

/// Starts `start`'s agent in its chat's folder, with a token for the core's MCP that ends with
/// the agent.
fn launch(
    roots: &Workspaces,
    places: &Places,
    policy: &SpawnPolicy,
    app: &App,
    start: &Start,
) -> Result<Granted, Fault> {
    let cwd = work_dir(places.work_root.as_deref(), &start.workspace, &start.chat)?;
    let launch = roots.launch(&start.workspace, start.agent, cwd)?;
    workspaces::require_sign_in(&launch)?;
    let (launch, revoke) = mcp::grant(places.mcp.as_ref(), launch, start)?;
    let agent = launch.start(policy, app, start.run.clone(), start.session.as_ref())?;
    Ok(Granted {
        agent,
        _revoke: revoke,
    })
}

/// The name of a chat's own files and folders.
pub(crate) fn file_stem(chat: &ChatId) -> String {
    crate::names::case_safe_name("c-", chat.as_str())
}

// Until a chat belongs to a project with a folder of its own, its agent works in one made for
// it under the work folder, where it can't reach the core's state.
fn work_dir(
    work_root: Option<&Path>,
    workspace: &WorkspaceId,
    chat: &ChatId,
) -> Result<PathBuf, Fault> {
    let failed = || {
        Fault::new(
            code!("chat.folder_failed"),
            "The chat's working folder couldn't be created",
        )
    };
    let dir = work_root
        .ok_or_else(failed)?
        .join(crate::names::case_safe_name("w-", workspace.as_str()))
        .join(file_stem(chat));
    platform::private_dir(&dir).map_err(|error| failed().because(error))?;
    Ok(dir)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AgentKind, ChatAnswerKind, ChatEvent, ChatQueryKind, Outcome, Receipt};
    use weacon_testing::{TempDir, recording_logger};

    use super::test_support::{chat, client, prompt, query, workspace};
    use super::*;
    use crate::test_support::VERSION;

    #[test]
    fn a_chat_works_in_a_private_folder_of_its_own_under_the_work_folder() {
        let dir = TempDir::new().unwrap();
        let root = dir.join("Weacon");
        let work = root.join("w-w-1");

        let upper = work_dir(Some(&root), &workspace(), &chat("Ab")).unwrap();
        let lower = work_dir(Some(&root), &workspace(), &chat("ab")).unwrap();

        assert_eq!(upper, work.join("c-.ab"));
        assert_eq!(lower, work.join("c-ab"));
        assert!(upper.is_dir() && lower.is_dir());
        std::fs::write(work.join("c-x"), b"").unwrap();
        for root in [Some(root.as_path()), None] {
            let blocked = work_dir(root, &workspace(), &chat("x")).unwrap_err();
            assert_eq!(blocked.code().as_str(), "chat.folder_failed");
        }
    }

    #[test]
    fn a_real_launch_accepts_a_chats_folder() {
        let dir = TempDir::new().unwrap();
        platform::plant_program(&dir.join("bin"), "claude", 0o755);
        let state = dir.join("state");
        let workspaces = Workspaces::new(&state, Some(dir.join("bin").into_os_string()));
        let cwd = work_dir(Some(&dir.join("Weacon")), &workspace(), &chat("c-1")).unwrap();

        let launch = workspaces.launch(&workspace(), AgentKind::Claude, cwd.clone());

        assert_eq!(launch.unwrap().cwd, cwd);
        // Where chats used to work: inside the state folder, so refused.
        let inside = workspaces.root(&workspace()).join("work");
        std::fs::create_dir_all(&inside).unwrap();
        let refused = workspaces.launch(&workspace(), AgentKind::Claude, inside);
        assert_eq!(
            refused.unwrap_err().code().as_str(),
            "workspace.cwd_refused"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_runs_token_lives_as_long_as_its_agent_and_a_failed_start_keeps_none() {
        let dir = TempDir::new().unwrap();
        // Signed in from its own folder when asked, and otherwise a run that waits; the second
        // isn't signed in.
        let signed_in = "[ \"$1\" = auth ] && { printf '{\"authMethod\":\"claude.ai\"}'; exit 0; }";
        platform::plant_script(
            &dir.join("bin"),
            "claude",
            &format!("{signed_in}\nexec sleep 30"),
        );
        let signed_out = "[ \"$1\" = auth ] && exit 1";
        platform::plant_script(
            &dir.join("signed-out"),
            "claude",
            &format!("{signed_out}\nexec sleep 30"),
        );
        platform::plant_program(&dir.join("broken"), "claude", 0o755);
        let app = weacon_appkit::build(&VERSION);
        let (log, _records) = recording_logger();
        let (mcp, _server) = mcp::start(&app, &log).unwrap();
        let places = |bin: &str| Places {
            path: Some(dir.join(bin).into_os_string()),
            work_root: Some(dir.join("Weacon")),
            mcp: Some(mcp.clone()),
        };
        let start = Start {
            workspace: workspace(),
            chat: chat("c-1"),
            agent: AgentKind::Claude,
            run: weacon_protocol::RunId::try_from("run-1".to_owned()).unwrap(),
            session: None,
        };
        let run = |bin: &str| {
            let places = places(bin);
            let roots = Workspaces::new(&dir.join("state"), places.path.clone());
            launch(&roots, &places, &SpawnPolicy::default(), &app, &start)
        };

        let agent = run("bin").unwrap();
        assert_eq!(mcp.grants.live(), 1);
        drop(agent);
        assert_eq!(mcp.grants.live(), 0);
        run("broken").map(drop).unwrap_err();
        assert_eq!(mcp.grants.live(), 0);
        let refused = run("signed-out").map(drop).unwrap_err();
        assert_eq!(refused.code().as_str(), "agent.signed_out");
        assert_eq!(mcp.grants.live(), 0);
    }

    #[test]
    fn the_clock_is_todays() {
        assert!(unix_now() > 1_700_000_000);
    }

    #[tokio::test]
    async fn the_daemons_engine_starts_agents_in_the_chats_folder_or_fails_the_prompt() {
        let dir = TempDir::new().unwrap();
        let app = weacon_appkit::build(&VERSION);
        let (log, _records) = recording_logger();
        let places = Places {
            path: None,
            work_root: Some(dir.join("Weacon")),
            mcp: None,
        };
        let (chats, engine) = start(&dir, places, app, SpawnPolicy::default(), log).unwrap();
        tokio::spawn(engine.run());
        // A file where the chat's working folder would go: its agent can't be started.
        let work = dir.join("Weacon/w-w-1");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("c-c-1"), b"").unwrap();

        chats.command(prompt("r-1", "Hi")).await.unwrap();

        let (anyone, _pushes) = client(1);
        let snapshot = query("c-1", ChatQueryKind::Snapshot);
        let Ok(Some(ChatAnswerKind::Snapshot { events, .. })) =
            chats.query(snapshot, &anyone).await
        else {
            panic!("no snapshot");
        };
        let failed = ChatEvent::Receipt {
            receipt: Receipt {
                request: prompt("r-1", "Hi").request,
                outcome: Outcome::Failed {
                    code: code!("chat.folder_failed"),
                },
            },
        };
        assert_eq!(events.last().map(|entry| &entry.event), Some(&failed));
        let journal = Runs::load(&dir).unwrap();
        let record = journal.of(&workspace(), &chat("c-1")).unwrap();
        assert!(record.active_at > 1_700_000_000);
    }
}
