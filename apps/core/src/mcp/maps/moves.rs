//! The checks of what each move of a repository's base did to its workspace's docs: one at a
//! time and in the order the moves came, so a later check never lands before an earlier one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::mpsc;
use weacon_codemap::{Facts, Index, assemble};
use weacon_observability::{Field, Logger};
use weacon_protocol::{CommitId, WorkspaceId};

use super::{Maps, Stores, Watched};
use crate::codemap::{Move, off_runtime};
use crate::proposals::{self, Proposals};

/// How many bases of failed checks a repository's next check starts from, at most.
const KEPT_BASES: usize = 4;

/// A repository, by its workspace and main worktree.
pub(super) type Repository = (WorkspaceId, PathBuf);

/// The commit each repository's base last moved to, and the queue that checks each move.
#[derive(Clone, Debug)]
pub(super) struct Moves {
    seen: Arc<Mutex<HashMap<Repository, CommitId>>>,
    checks: mpsc::UnboundedSender<(Repository, Move, Logger)>,
}

impl Maps {
    /// These maps, proposing what each move of a watched repository's base did to its
    /// workspace's docs through `proposals`.
    pub(crate) fn proposing(self, proposals: Proposals) -> Self {
        Self {
            proposals: Some(proposals),
            ..self
        }
    }

    /// Where the MCP's runs propose, if anywhere.
    pub(crate) fn proposals(&self) -> Option<Proposals> {
        self.proposals.clone()
    }
}

impl Stores {
    /// Queues `moved`, to propose what it did to the anchors of `watched`'s workspace's docs,
    /// once for each commit a repository's base moves to.
    pub(super) fn moved(&self, watched: &Watched, moved: Move, log: &Logger) {
        if let Some(moves) = &self.moves {
            let key = (watched.workspace.clone(), watched.main.clone());
            moves.queue(key, moved, log);
        }
    }
}

impl Moves {
    /// Checks moves on a task of its own, proposing through `proposals`; needs a runtime.
    pub(super) fn start(proposals: Proposals) -> Self {
        let (checks, queued) = mpsc::unbounded_channel();
        tokio::spawn(check_moves(proposals, queued));
        Self {
            seen: Arc::default(),
            checks,
        }
    }

    /// Queues `moved` of `key`, once for each commit its base moves to.
    pub(super) fn queue(&self, key: Repository, moved: Move, log: &Logger) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.get(&key) == Some(&moved.commit) {
            return;
        }
        seen.insert(key.clone(), moved.commit.clone());
        drop(seen);
        drop(self.checks.send((key, moved, log.clone())));
    }
}

/// Checks each queued move in turn; a move whose check failed is checked again, from its
/// base, with the repository's next move.
async fn check_moves(
    proposals: Proposals,
    mut queued: mpsc::UnboundedReceiver<(Repository, Move, Logger)>,
) {
    let mut failed: HashMap<Repository, Vec<Vec<Facts>>> = HashMap::new();
    while let Some((key, moved, log)) = queued.recv().await {
        // A pin re-anchored by a check that failed for another doc is from a later base.
        let mut bases = failed.remove(&key).unwrap_or_default();
        bases.push(moved.was);
        let kept = bases.clone();
        let (now, commit) = (moved.now, moved.commit);
        let assembled = move || {
            let was: Vec<Index> = bases.iter().map(|base| assemble(base)).collect();
            Ok((was, assemble(&now), commit))
        };
        let checked = async {
            let (was, now, commit) = off_runtime(assembled).await?;
            let was: Vec<&Index> = was.iter().collect();
            proposals::moved(&proposals, &key.0, &was, &now, &commit).await
        };
        match checked.await {
            Ok(docs) => log.info("anchors checked", &[("docs", docs.into())]),
            Err(fault) => {
                log.warn("anchors check failed", &[("err", Field::error(&fault))]);
                failed.insert(key, kept_bases(kept));
            }
        }
    }
}

/// The oldest of `bases`, which holds at least one, and the latest of the rest, at most
/// `KEPT_BASES` in all.
fn kept_bases(mut bases: Vec<Vec<Facts>>) -> Vec<Vec<Facts>> {
    let over = bases.len().saturating_sub(KEPT_BASES);
    bases.drain(1..over.saturating_add(1));
    bases
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(files: &[(&str, &str)]) -> Vec<Facts> {
        let mut reader = weacon_codemap::Reader::new().unwrap();
        files
            .iter()
            .filter_map(|(path, text)| {
                let file = weacon_codemap::SourceFile {
                    path: (*path).to_owned(),
                    text: (*text).to_owned(),
                };
                reader.facts(&file).unwrap()
            })
            .collect()
    }

    #[test]
    fn a_repository_keeps_its_oldest_failed_base_and_the_latest() {
        let bases = |count: usize| -> Vec<Vec<Facts>> {
            (0..count)
                .map(|n| facts(&[("src/lib.rs", &format!("pub fn f{n}() {{}}\n"))]))
                .collect()
        };
        let all = bases(6);

        assert_eq!(kept_bases(bases(1)), bases(1));
        assert_eq!(kept_bases(bases(KEPT_BASES)), bases(KEPT_BASES));
        let kept = kept_bases(all.clone());
        assert_eq!(kept.len(), KEPT_BASES);
        assert_eq!(kept[0], all[0]);
        assert_eq!(kept[1..], all[3..]);
    }
}
