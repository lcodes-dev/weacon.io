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

use weacon_agents::{AgentRun, SpawnPolicy};
use weacon_observability::{Fault, Logger};
use weacon_protocol::{App, ChatId, code};

pub(crate) use engine::Chats;
use engine::{Engine, Host, Start};

use crate::platform;
use crate::runs::Runs;
use crate::workspaces::Workspaces;

/// The clients' way to the chats in `state_dir`, and the engine to run behind it, which starts
/// each agent under `policy` with its workspace's own sign-in, finding vendors on `path`.
///
/// # Errors
/// When the journal of runs can't be read.
pub(crate) fn start(
    state_dir: &Path,
    path: Option<OsString>,
    app: App,
    policy: SpawnPolicy,
    log: Logger,
) -> Result<(Chats, Engine<AgentRun>), Fault> {
    let runs = Runs::load(state_dir)?;
    let workspaces = Workspaces::new(state_dir, path);
    let roots = workspaces.clone();
    let host = Host {
        start: Arc::new(move |start: &Start| {
            let cwd = work_dir(&roots.root(&start.workspace), &start.chat)?;
            let launch = roots.launch(&start.workspace, start.agent, cwd)?;
            launch.start(&policy, &app, start.run.clone(), start.session.as_ref())
        }),
        now: unix_now,
    };
    Ok(Engine::new(runs, workspaces, host, log))
}

/// The name of a chat's own files and folders.
pub(crate) fn file_stem(chat: &ChatId) -> String {
    platform::case_safe_name("c-", chat.as_str())
}

// Until a chat belongs to a project with a folder of its own, its agent works in one made for
// it under the workspace's folder.
fn work_dir(workspace_root: &Path, chat: &ChatId) -> Result<PathBuf, Fault> {
    let dir = workspace_root.join("work").join(file_stem(chat));
    platform::private_dir(&dir).map_err(|error| {
        Fault::new(
            code!("chat.folder_failed"),
            "The chat's working folder couldn't be created",
        )
        .because(error)
    })?;
    Ok(dir)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatAnswerKind, ChatEvent, ChatQueryKind, Outcome, Receipt};
    use weacon_testing::{TempDir, recording_logger};

    use super::test_support::{chat, client, prompt, query, workspace};
    use super::*;
    use crate::test_support::VERSION;

    #[test]
    fn a_chat_works_in_a_private_folder_of_its_own_under_its_workspace() {
        let dir = TempDir::new().unwrap();
        let root = dir.join("w-one");

        let upper = work_dir(&root, &chat("Ab")).unwrap();
        let lower = work_dir(&root, &chat("ab")).unwrap();

        assert_eq!(upper, root.join("work").join("c-.ab"));
        assert_eq!(lower, root.join("work").join("c-ab"));
        assert!(upper.is_dir() && lower.is_dir());
        std::fs::write(root.join("work/c-x"), b"").unwrap();
        let blocked = work_dir(&root, &chat("x")).unwrap_err();
        assert_eq!(blocked.code().as_str(), "chat.folder_failed");
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
        let (chats, engine) = start(&dir, None, app, SpawnPolicy::default(), log).unwrap();
        tokio::spawn(engine.run());
        // A file where the chat's working folder would go: its agent can't be started.
        let work = Workspaces::new(&dir, None).root(&workspace()).join("work");
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
