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
        let ours = place(worktree);
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
            .find(|dir| {
                let named = names(dir).and_then(|git_file| place(git_file.parent()?));
                ours.is_some() && named == ours
            })
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

// Where `folder` is: its parent resolved, but not `folder` itself, which an agent could replace
// with a link to another worktree, as it could its `.git`.
fn place(folder: &Path) -> Option<PathBuf> {
    Some(resolved(folder.parent()?)?.join(folder.file_name()?))
}

// The `.git` file of the worktree a git folder of `worktrees/` belongs to. Git may write it
// relative to that folder.
fn names(dir: &Path) -> Option<PathBuf> {
    let named = fs::read_to_string(dir.join("gitdir")).ok()?;
    Some(dir.join(named.trim_end()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, git, repo};

    #[test]
    fn a_git_file_linked_to_another_worktrees_never_borrows_its_folder() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let [linked, other] = ["a", "b"].map(|id| {
            let path = dir.join(format!("trees/c-{id}"));
            git.chat_worktree(&repo, &chat(id), &path).unwrap();
            path
        });
        fs::remove_file(linked.join(".git")).unwrap();
        if !crate::platform::plant_link(&other.join(".git"), &linked.join(".git")) {
            return;
        }

        let folder = git
            .in_tree(&repo, &linked)
            .unwrap()
            .folder()
            .unwrap()
            .to_owned();

        assert_eq!(
            place(names(&folder).unwrap().parent().unwrap()),
            place(&linked)
        );
    }

    #[test]
    fn each_worktree_gets_its_own_git_folder() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let trees = ["a", "b"].map(|id| {
            let path = dir.join(format!("trees/c-{id}"));
            git.chat_worktree(&repo, &chat(id), &path).unwrap();
            path
        });

        for tree in trees {
            let told = git.in_tree(&repo, &tree).unwrap();
            let folder = told.folder().unwrap();
            assert_eq!(
                resolved(&names(folder).unwrap()),
                resolved(&tree.join(".git"))
            );
        }
        let error = git.in_tree(&repo, &dir.join("trees/c-x")).unwrap_err();
        assert_eq!(error.code().as_str(), "worktree.unregistered");
    }
}
