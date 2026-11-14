//! The daemon's answers about chats' worktrees. Each is sent once git is done, from a task of
//! its own, so a slow git never holds up the connection or the chats' events on it.

use std::future::Future;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use tokio::sync::mpsc;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    DiffQuery, Envelope, ErrorBody, Message, WorktreeAnswer, WorktreeCommand, WorktreeQuery,
};

use super::Shared;
use crate::chats::Places;
use crate::git::service::Worktrees;
use crate::platform;
use crate::platform::trash::Trashed;
use crate::session::Reply;

/// Where replies that come after their request go, each with its request's envelope id.
pub(super) type Late = mpsc::Sender<Envelope>;

/// How many late replies wait for the connection before their tasks wait too.
pub(super) const LATE: usize = 64;

const NONE: Reply = Reply {
    envelope: None,
    close: false,
};

pub(super) fn worktrees(state_dir: &Path, places: &Places) -> Worktrees {
    let (path, work_root) = (places.path.clone(), places.work_root.clone());
    Worktrees::new(state_dir, path, work_root, to_trash)
}

fn to_trash(path: &Path, now: SystemTime) -> io::Result<Trashed> {
    platform::trash::trash(path, &|key| std::env::var_os(key), now)
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
    NONE
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
    let worktrees = shared.worktrees.clone();
    send(
        shared,
        id,
        late,
        async move { worktrees.command(command).await },
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
    NONE
}

async fn refuse(log: &Logger, id: u32, fault: &Fault, late: &Late) {
    log.warn("worktree request refused", &[("err", Field::error(fault))]);
    let message = Message::Error(ErrorBody {
        code: fault.code().clone(),
        retryable: false,
    });
    let _closed = late.send(Envelope { id, message }).await;
}
