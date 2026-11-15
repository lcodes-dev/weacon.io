//! A chat's worktree for its agent's next run, on the chat's branch of the repository its first
//! prompt named.

use std::path::{Path, PathBuf};

use weacon_observability::Fault;
use weacon_protocol::{RepoId, code};

use super::state::{Stores, hold_worktrees};
use super::{Git, Start, branch, run, text};
use crate::chats::chat_folder;
use crate::runs::ChatKey;
use crate::workspaces::Workspaces;

/// The folder `key`'s agent works in: its worktree of `repo` under `work_root`, made the first
/// time and recorded with where its branch began. One in the Trash is refused until it's back.
pub(crate) fn chat_tree(
    workspaces: &Workspaces,
    work_root: &Path,
    key: &ChatKey,
    repo: &RepoId,
) -> Result<PathBuf, Fault> {
    let stores = Stores::new(workspaces, &key.workspace);
    let folder = chat_folder(work_root, &key.workspace, &key.chat);
    let recorded = stores.chats().get(&key.chat)?;
    // A chat that worked in a plain folder before it was given worktrees keeps it.
    if recorded.is_none() && folder.is_dir() && !folder.join(".git").exists() {
        return Ok(folder);
    }
    let found = stores.repo(repo)?;
    let git = Git::open(workspaces.state_dir(), workspaces.path())?;
    if stores.removed(git.clone()).removed(&key.chat)?.is_some() {
        return Err(Fault::new(
            code!("worktree.in_trash"),
            "The chat's worktree is in the Trash; restore it to go on",
        ));
    }
    let _held = hold_worktrees(workspaces.state_dir())?;
    let tree = git.chat_worktree(&found.path, &key.chat, &folder)?;
    if recorded.is_none() {
        let start = match tree.start {
            Some(start) => start,
            // Made before, but its record was lost.
            None => start_again(&git, &found.path, &branch(&key.chat))?,
        };
        stores.chats().record(&key.chat, found.id, start)?;
    }
    Ok(folder)
}

// Where `branch` began, worked out again: where it meets the branch `repo` has checked out.
fn start_again(git: &Git, repo: &Path, branch: &str) -> Result<Start, Fault> {
    let full = format!("refs/heads/{branch}");
    let met = text(run(git.command(repo).args(["merge-base", "HEAD", &full]))?)?;
    Ok(Start {
        commit: met.trim_end().to_owned(),
        branch: git.checked_out(repo),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::WorkspaceId;
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
        let workspaces = Workspaces::new(&dir.join("state"), std::env::var_os("PATH"));
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
            chat_tree(&self.workspaces, &self.dir.join("work"), &self.key, repo)
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
    fn first_runs_of_two_chats_at_once_are_both_recorded() {
        let bench = bench();
        let other = ChatKey::new(&bench.key.workspace, &chat("c-2"));
        let work = bench.dir.join("work");

        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| bench.tree(&bench.site));
            let second = scope.spawn(|| chat_tree(&bench.workspaces, &work, &other, &bench.site));
            (first.join().unwrap(), second.join().unwrap())
        });

        first.unwrap();
        second.unwrap();
        let chats = bench.stores().chats();
        assert!(chats.get(&bench.key.chat).unwrap().is_some());
        assert!(chats.get(&other.chat).unwrap().is_some());
    }

    #[test]
    fn a_worktree_is_made_only_under_the_cores_hold() {
        let bench = bench();
        // A folder where the hold's lock file goes, so the hold can't be taken.
        fs::create_dir_all(bench.dir.join("state/git/worktrees.lock")).unwrap();

        let error = bench.tree(&bench.site).unwrap_err();

        assert_eq!(error.code().as_str(), "git.hold_failed");
        assert!(!bench.dir.join("work/w-w-1/c-c-1").exists());
    }

    #[test]
    fn a_worktree_whose_record_was_lost_is_recorded_again() {
        let bench = bench();
        let first = bench.tree(&bench.site).unwrap();
        let chats = bench.stores().chats();
        let made = chats.get(&bench.key.chat).unwrap().unwrap();
        fs::remove_file(bench.dir.join("state/workspaces/w-w-1/git/chats.json")).unwrap();

        assert_eq!(bench.tree(&bench.site).unwrap(), first);

        assert_eq!(chats.get(&bench.key.chat).unwrap(), Some(made));
    }

    #[test]
    fn a_chat_that_worked_in_a_plain_folder_keeps_it() {
        let bench = bench();
        let plain = bench.dir.join("work/w-w-1/c-c-1");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("notes.md"), "mine\n").unwrap();

        // Even with no git on the way and a repository not registered.
        let no_git = Workspaces::new(&bench.dir.join("state"), None);
        let unknown = RepoId::try_from("nowhere".to_owned()).unwrap();
        let kept = chat_tree(&no_git, &bench.dir.join("work"), &bench.key, &unknown).unwrap();

        assert_eq!(kept, plain);
        assert_eq!(bench.tree(&bench.site).unwrap(), plain);
        assert!(!plain.join(".git").exists());
        assert_eq!(bench.stores().chats().get(&bench.key.chat).unwrap(), None);
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
        bench
            .stores()
            .removed(told)
            .remove(&bench.key.chat, &first, to_trash, std::time::UNIX_EPOCH)
            .unwrap();

        let error = bench.tree(&bench.site).unwrap_err();

        assert_eq!(error.code().as_str(), "worktree.in_trash");
        assert!(!first.exists());
    }
}
