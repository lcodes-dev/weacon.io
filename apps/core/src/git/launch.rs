//! A chat's worktree for its agent's next run, on the chat's branch of the repository its first
//! prompt named.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use weacon_observability::Fault;
use weacon_protocol::{RepoId, code};

use super::Git;
use super::state::Stores;
use crate::chats::chat_folder;
use crate::runs::ChatKey;
use crate::workspaces::Workspaces;

/// The folder `key`'s agent works in: its worktree of `repo` under `work_root`, made the first
/// time and recorded with where its branch began. One in the Trash is refused until it's back.
pub(crate) fn chat_tree(
    workspaces: &Workspaces,
    path: Option<&OsStr>,
    work_root: &Path,
    key: &ChatKey,
    repo: &RepoId,
) -> Result<PathBuf, Fault> {
    let stores = Stores::new(workspaces, &key.workspace);
    let found = stores.repo(repo)?;
    let git = Git::open(workspaces.state_dir(), path)?;
    if stores.removed(git.clone()).removed(&key.chat)?.is_some() {
        return Err(Fault::new(
            code!("worktree.in_trash"),
            "The chat's worktree is in the Trash; restore it to go on",
        ));
    }
    let folder = chat_folder(work_root, &key.workspace, &key.chat);
    let tree = git.chat_worktree(&found.path, &key.chat, &folder)?;
    if let Some(start) = tree.start {
        stores.chats().record(&key.chat, found.id, start)?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::{ChatId, WorkspaceId};
    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, fixture, git, repo};
    use crate::platform::trash::Trashed;

    struct Bench {
        dir: TempDir,
        workspaces: Workspaces,
        key: ChatKey,
        site: RepoId,
    }

    fn bench() -> Bench {
        let dir = TempDir::new().unwrap();
        let workspaces = Workspaces::new(&dir.join("state"), None);
        let workspace = WorkspaceId::try_from("w-1".to_owned()).unwrap();
        let key = ChatKey::new(&workspace, &chat("c-1"));
        let site = RepoId::try_from("site".to_owned()).unwrap();
        let folder = repo(&dir);
        Stores::new(&workspaces, &workspace)
            .repos()
            .add(&git(&dir), site.clone(), &folder)
            .unwrap();
        Bench {
            dir,
            workspaces,
            key,
            site,
        }
    }

    impl Bench {
        fn tree(&self, repo: &RepoId) -> Result<PathBuf, Fault> {
            let path = std::env::var_os("PATH");
            let work = self.dir.join("work");
            chat_tree(&self.workspaces, path.as_deref(), &work, &self.key, repo)
        }

        fn stores(&self) -> Stores {
            Stores::new(&self.workspaces, &self.key.workspace)
        }
    }

    #[test]
    fn a_chats_first_run_makes_its_worktree_and_later_ones_find_it() {
        let bench = bench();

        let first = bench.tree(&bench.site).unwrap();
        fs::write(first.join("agent.txt"), "mine\n").unwrap();
        let again = bench.tree(&bench.site).unwrap();

        assert_eq!(again, first);
        assert!(again.join("agent.txt").exists());
        let head = fs::read_to_string(first.join(".git")).unwrap();
        assert!(head.starts_with("gitdir: "), "{head}");
        let record = bench
            .stores()
            .chats()
            .get(&bench.key.chat)
            .unwrap()
            .unwrap();
        assert_eq!(record.repo, bench.site);
        assert_eq!(record.start.branch.as_deref(), Some("main"));
        fixture(
            &bench.dir,
            &first,
            &["rev-parse", "--verify", "refs/heads/weacon/c-1"],
        );
    }

    #[test]
    fn a_repository_the_workspace_never_registered_is_refused() {
        let bench = bench();
        let other = RepoId::try_from("other".to_owned()).unwrap();

        let error = bench.tree(&other).unwrap_err();

        assert_eq!(error.code().as_str(), "repo.unknown");
        assert!(!bench.dir.join("work").exists());
    }

    #[test]
    fn a_worktree_in_the_trash_is_restored_before_it_runs_again() {
        let bench = bench();
        let first = bench.tree(&bench.site).unwrap();
        let told = git(&bench.dir)
            .in_tree(&bench.dir.join("repo"), &first)
            .unwrap();
        let bin = bench.dir.join("bin");
        let to_trash = |path: &Path| {
            fs::create_dir_all(&bin)?;
            let item = bin.join("c-1");
            fs::rename(path, &item)?;
            Ok(Trashed { item, info: None })
        };
        let chat: &ChatId = &bench.key.chat;
        bench
            .stores()
            .removed(told)
            .remove(chat, &first, to_trash, std::time::UNIX_EPOCH)
            .unwrap();

        let error = bench.tree(&bench.site).unwrap_err();

        assert_eq!(error.code().as_str(), "worktree.in_trash");
        assert!(!first.exists());
    }
}
