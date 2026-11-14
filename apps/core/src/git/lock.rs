//! Weacon's lock on a worktree in the Trash, which keeps `git worktree prune` from forgetting
//! it. Each runs from the worktree's git folder when this git has one, so it works with the
//! worktree's own folder gone.

use std::path::Path;
use std::process::Command;

use weacon_observability::Fault;
use weacon_protocol::code;

use super::{Git, absolute, records, run, text};
use crate::workspaces::resolved;

// Weacon's lock says this; one that says anything else is the person's.
const REASON: &str = "In the Trash";

impl Git {
    /// Locks `worktree`; one Weacon already locked stays so, and one the person locked is
    /// refused.
    pub(crate) fn lock(&self, worktree: &Path) -> Result<(), Fault> {
        match self.lock_reason(worktree)? {
            Some(reason) if reason == REASON => Ok(()),
            Some(_) => Err(Fault::new(
                code!("git.locked_by_person"),
                "The worktree is locked for another reason",
            )),
            None => run(self
                .lock_command(worktree)
                .args(["worktree", "lock", "--reason", REASON, "--"])
                .arg(worktree))
            .map(drop),
        }
    }

    /// Takes Weacon's lock off `worktree`; any other lock stays.
    pub(crate) fn unlock(&self, worktree: &Path) -> Result<(), Fault> {
        if self.lock_reason(worktree)?.as_deref() != Some(REASON) {
            return Ok(());
        }
        run(self
            .lock_command(worktree)
            .args(["worktree", "unlock", "--"])
            .arg(worktree))
        .map(drop)
    }

    /// Why git has `worktree` locked, if it does.
    pub(super) fn lock_reason(&self, worktree: &Path) -> Result<Option<String>, Fault> {
        absolute(&[worktree])?;
        let listing = text(run(self.lock_command(worktree).args([
            "worktree",
            "list",
            "--porcelain",
            "-z",
        ]))?)?;
        let Some(ours) = resolved(worktree) else {
            return Ok(None);
        };
        Ok(records(&listing)
            .find(|listed| resolved(&listed.tree.path).as_ref() == Some(&ours))
            .and_then(|listed| listed.locked))
    }

    fn lock_command(&self, worktree: &Path) -> Command {
        match &self.folder {
            Some(dir) => self.command_in(dir, None),
            None => self.command(worktree),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, fixture, git, repo};

    fn made(dir: &Path) -> (Git, std::path::PathBuf, std::path::PathBuf) {
        let (git, repo) = (git(dir), repo(dir));
        let tree = dir.join("trees/c-a");
        git.chat_worktree(&repo, &chat("a"), &tree).unwrap();
        (git, repo, tree)
    }

    #[test]
    fn a_lock_or_unlock_already_done_is_done() {
        let dir = TempDir::new().unwrap();
        let (git, _, tree) = made(&dir);

        git.unlock(&tree).unwrap();
        assert_eq!(git.lock_reason(&tree).unwrap(), None);
        git.lock(&tree).unwrap();
        git.lock(&tree).unwrap();
        assert_eq!(git.lock_reason(&tree).unwrap().as_deref(), Some(REASON));
        git.unlock(&tree).unwrap();
        assert_eq!(git.lock_reason(&tree).unwrap(), None);
    }

    #[test]
    fn the_persons_own_lock_is_never_taken_or_lifted() {
        let dir = TempDir::new().unwrap();
        let (git, repo, tree) = made(&dir);
        fixture(&dir, &repo, &["worktree", "lock", tree.to_str().unwrap()]);

        let error = git.lock(&tree).unwrap_err();
        git.unlock(&tree).unwrap();

        assert_eq!(error.code().as_str(), "git.locked_by_person");
        assert_eq!(git.lock_reason(&tree).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn a_worktree_whose_folder_is_gone_is_still_unlocked() {
        let dir = TempDir::new().unwrap();
        let (git, repo, tree) = made(&dir);
        let told = git.in_tree(&repo, &tree).unwrap();
        told.lock(&tree).unwrap();
        fs::rename(&tree, dir.join("elsewhere")).unwrap();

        told.unlock(&tree).unwrap();

        assert_eq!(told.lock_reason(&tree).unwrap(), None);
    }

    #[test]
    fn a_relative_path_is_refused() {
        let dir = TempDir::new().unwrap();
        let git = git(&dir);

        for relative in ["trees/c-a", "--force"] {
            let path = Path::new(relative);
            for error in [git.lock(path).unwrap_err(), git.unlock(path).unwrap_err()] {
                assert_eq!(error.code().as_str(), "git.path_relative", "{relative}");
            }
        }
    }
}
