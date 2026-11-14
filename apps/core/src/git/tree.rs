//! Git for one chat's worktree, told its repository outright. The worktree's own `.git` file
//! says where its repository is, but an agent can rewrite it to point at config of its own, which
//! would then run with the person's credentials, so it is never read.

use std::fs;
use std::path::{Path, PathBuf};

use weacon_observability::Fault;
use weacon_protocol::code;

use super::{Git, resolved, run, text};

impl Git {
    /// Git that runs in `worktree` with the git folder `main`'s repository keeps for it.
    pub(crate) fn in_tree(&self, main: &Path, worktree: &Path) -> Result<Self, Fault> {
        let common = text(run(self.command(main).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ]))?)?;
        let ours = resolved(&worktree.join(".git"));
        let unknown = || {
            Fault::new(
                code!("worktree.unregistered"),
                "The repository doesn't list the chat's worktree",
            )
        };
        let entries = fs::read_dir(Path::new(common.trim_end()).join("worktrees"))
            .map_err(|error| unknown().because(error))?;
        let dir = entries
            .flatten()
            .map(|entry| entry.path())
            .find(|dir| names(dir).is_some_and(|named| ours.is_some() && resolved(&named) == ours))
            .ok_or_else(unknown)?;
        Ok(Self {
            folder: Some(dir),
            ..self.clone()
        })
    }

    /// The git folder this git is told to use, if it is told one.
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }
}

// The `.git` file of the worktree a git folder of `worktrees/` belongs to.
fn names(dir: &Path) -> Option<PathBuf> {
    let named = fs::read_to_string(dir.join("gitdir")).ok()?;
    Some(PathBuf::from(named.trim_end()))
}
