//! The daemon's answers about chats' worktrees. Each is sent once git is done, from a task of
//! its own, so a slow git never holds up the connection or the chats' events on it.

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::Path;

use tokio::sync::mpsc;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    DiffQuery, Envelope, ErrorBody, Message, RunState, WorktreeAction, WorktreeAnswer,
    WorktreeCommand, WorktreeQuery, code,
};

use super::Shared;
use crate::chats::{Chats, Places};
use crate::git::service::Worktrees;
use crate::platform;
use crate::runs::ChatKey;
use crate::session::Reply;

/// Where replies that come after their request go, each with its request's envelope id.
pub(super) type Late = mpsc::Sender<Envelope>;

/// How many late replies wait for the connection before their tasks wait too.
pub(super) const LATE: usize = 64;

pub(super) fn worktrees(state_dir: &Path, places: &Places) -> Worktrees {
    let (path, work_root) = (places.path.clone(), places.work_root.clone());
    Worktrees::new(state_dir, path, work_root, open_terminal)
}

// The terminal runs this very core, so it's the same build that answered.
fn open_terminal(folder: &Path, args: &[OsString]) -> io::Result<bool> {
    let run: Vec<OsString> = std::iter::once(std::env::current_exe()?.into_os_string())
        .chain(args.iter().cloned())
        .collect();
    let Some(mut command) = platform::terminal_in(folder, &|key| std::env::var_os(key), &run)
    else {
        return Ok(false);
    };
    let mut child = command.spawn()?;
    // Reaped once the terminal's own launcher exits, which may be when its window closes.
    std::thread::spawn(move || child.wait());
    Ok(true)
}

/// Sends a chat's changes, one file to an answer. Each of these replies later, so none now.
pub(super) fn diff(shared: &Shared, id: u32, query: DiffQuery, late: &Late) -> Reply {
    let (worktrees, log, late) = (shared.worktrees.clone(), shared.log.clone(), late.clone());
    tokio::spawn(async move {
        let answers = match worktrees.diff(query).await {
            Ok(answers) => answers,
            Err(fault) => return refuse(&log, id, &fault, &late).await,
        };
        for answer in answers {
            let message = Message::DiffAnswer(answer);
            if late.send(Envelope { id, message }).await.is_err() {
                return;
            }
        }
    });
    Reply::NOTHING
}

pub(super) fn query(shared: &Shared, id: u32, query: WorktreeQuery, late: &Late) -> Reply {
    let worktrees = shared.worktrees.clone();
    send(
        shared,
        id,
        late,
        async move { worktrees.query(query).await },
    )
}

pub(super) fn command(shared: &Shared, id: u32, command: WorktreeCommand, late: &Late) -> Reply {
    let (worktrees, chats) = (shared.worktrees.clone(), shared.chats.clone());
    send(shared, id, late, async move {
        act(&chats, &worktrees, command).await
    })
}

/// What `command` does to its chat's worktree, from the socket or relayed; a removal is
/// refused while the chat's agent is on a turn.
pub(super) async fn act(
    chats: &Chats,
    worktrees: &Worktrees,
    command: WorktreeCommand,
) -> Result<WorktreeAnswer, Fault> {
    if command.action == WorktreeAction::Remove {
        let key = ChatKey::new(&command.workspace, &command.chat);
        let listed = chats.list().await?;
        let run = listed.iter().find(|chat| chat.key == key);
        if run.is_some_and(|chat| on_a_turn(chat.run.as_ref())) {
            return Err(Fault::new(
                code!("worktree.busy"),
                "Stop the chat's agent before removing its worktree",
            ));
        }
    }
    worktrees.command(command).await
}

// Whether a run is writing in its worktree, or about to.
fn on_a_turn(run: Option<&RunState>) -> bool {
    matches!(
        run,
        Some(RunState::Starting | RunState::Running | RunState::Waiting)
    )
}

fn send(
    shared: &Shared,
    id: u32,
    late: &Late,
    answer: impl Future<Output = Result<WorktreeAnswer, Fault>> + Send + 'static,
) -> Reply {
    let (log, late) = (shared.log.clone(), late.clone());
    tokio::spawn(async move {
        match answer.await {
            Ok(answer) => {
                let message = Message::WorktreeAnswer(answer);
                // A closed connection wants nothing more.
                let _closed = late.send(Envelope { id, message }).await;
            }
            Err(fault) => refuse(&log, id, &fault, &late).await,
        }
    });
    Reply::NOTHING
}

async fn refuse(log: &Logger, id: u32, fault: &Fault, late: &Late) {
    log.warn("worktree request refused", &[("err", Field::error(fault))]);
    let message = Message::Error(ErrorBody {
        code: fault.code().clone(),
        retryable: false,
    });
    let _closed = late.send(Envelope { id, message }).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_run_on_a_turn_keeps_its_worktree_from_being_removed() {
        for state in [RunState::Starting, RunState::Running, RunState::Waiting] {
            assert!(on_a_turn(Some(&state)), "{state:?}");
        }
        for state in [RunState::Idle, RunState::Sleeping, RunState::Ended] {
            assert!(!on_a_turn(Some(&state)), "{state:?}");
        }
        assert!(!on_a_turn(None));
    }
}
