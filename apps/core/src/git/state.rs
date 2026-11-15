//! The git module's own state files: each a JSON list, read whole and written whole, in the
//! workspace's folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use weacon_observability::Fault;
use weacon_protocol::{RepoId, WorkspaceId, code};

use super::Git;
use super::chats::ChatRepos;
use super::removal::Journal;
use super::repos::{Repo, Repos};
use crate::db::LocalRepo;
use crate::hold::{Hold, hold as take};
use crate::workspaces::Workspaces;
use crate::workspaces::resolved;

/// A workspace's git state: its repositories, its chats' worktrees, and those in the Trash.
pub(crate) struct Stores {
    workspace: WorkspaceId,
    folder: PathBuf,
}

impl Stores {
    pub(crate) fn new(workspaces: &Workspaces, workspace: &WorkspaceId) -> Self {
        let folder = workspaces.root(workspace).join("git");
        Self {
            workspace: workspace.clone(),
            folder,
        }
    }

    pub(crate) fn repos(&self) -> Repos {
        Repos::new(self.workspace.clone(), self.folder.join("repos.json"))
    }

    /// The registered repository `id`; one not registered is refused.
    pub(crate) fn repo(&self, id: &RepoId) -> Result<Repo, Fault> {
        self.repos().get(id)?.ok_or_else(|| {
            Fault::new(
                code!("repo.unknown"),
                "That repository isn't registered in the workspace",
            )
        })
    }

    pub(crate) fn chats(&self) -> ChatRepos {
        ChatRepos::new(self.workspace.clone(), self.folder.join("chats.json"))
    }

    pub(crate) fn removed(&self, git: Git) -> Journal {
        Journal::new(
            self.workspace.clone(),
            self.folder.join("removed.json"),
            git,
        )
    }
}

/// The list in `file`; none before the first is written.
pub(super) fn load<T: DeserializeOwned>(file: &Path) -> io::Result<Vec<T>> {
    match fs::read(file) {
        Ok(json) => Ok(serde_json::from_slice(&json)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub(super) fn save<T: Serialize>(file: &Path, items: &[T]) -> io::Result<()> {
    weacon_appkit::write_whole(file, &serde_json::to_vec(items)?)
}

/// The core's hold on changing the worktrees of the repository whose git folder is
/// `common_dir`, for every workspace, since one repository can be in several: git can fail when
/// two are added to a repository at once.
pub(super) fn hold_worktrees(state_dir: &Path, common_dir: &Path) -> Result<Hold, Fault> {
    take(&worktrees_lock(state_dir, common_dir)).map_err(|error| {
        Fault::new(code!("git.hold_failed"), "The worktrees couldn't be held").because(error)
    })
}

// Named by the git folder, which a repository registered as a linked worktree shares too.
// Resolved only here: on Windows a resolved path is one git can't be given.
pub(super) fn worktrees_lock(state_dir: &Path, common_dir: &Path) -> PathBuf {
    let common = resolved(common_dir).unwrap_or_else(|| common_dir.to_owned());
    let name = format!("{}.lock", LocalRepo::at(&common).as_str());
    state_dir.join("git/worktrees").join(name)
}

/// A hold on changing `file`, once another holding it, in this process or another, lets go.
pub(super) fn hold(file: &Path) -> io::Result<Hold> {
    take(&file.with_extension("lock"))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn worktree_holds_on_two_repositories_are_apart() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        let _one = hold_worktrees(&state, Path::new("/repo/one")).unwrap();

        // Taken on a thread of its own, so a shared hold fails the test rather than hangs it.
        let (taken, held) = mpsc::channel();
        thread::spawn(move || {
            drop(taken.send(hold_worktrees(&state, Path::new("/repo/two")).map(drop)));
        });

        let two = held.recv_timeout(Duration::from_secs(5)).unwrap();
        two.unwrap();
    }

    #[test]
    fn a_list_comes_back_as_saved_and_none_is_empty() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");

        assert_eq!(load::<u32>(&file).unwrap(), Vec::<u32>::new());
        save(&file, &[3_u32, 1]).unwrap();
        assert_eq!(load::<u32>(&file).unwrap(), [3, 1]);
    }

    // Each thread adds to a count read and written whole: none may be lost.
    #[test]
    fn holds_on_one_file_take_turns() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("state/count.json");
        let add = |file: PathBuf| {
            for _ in 0..50 {
                let _held = hold(&file).unwrap();
                let count = load::<u32>(&file).unwrap();
                save(&file, &[count.first().copied().unwrap_or(0) + 1]).unwrap();
            }
        };
        let adding: Vec<_> = (0..4)
            .map(|_| {
                let file = file.clone();
                thread::spawn(move || add(file))
            })
            .collect();

        for thread in adding {
            thread.join().unwrap();
        }
        assert_eq!(load::<u32>(&file).unwrap(), [200]);
    }

    // Run by the test below, in a process of its own: holds the file it's given until told, then
    // finds it untouched and marks it just before it lets go.
    #[test]
    #[ignore = "a helper the test below runs in another process"]
    fn held_in_another_process() {
        use std::io::{BufRead, Write};

        let Some(file) = std::env::var_os("WEACON_HOLD") else {
            return;
        };
        let file = PathBuf::from(file);
        let _held = hold(&file).unwrap();
        writeln!(io::stdout(), "held").unwrap();
        io::stdout().flush().unwrap();
        io::stdin().lock().read_line(&mut String::new()).unwrap();
        assert_eq!(load::<String>(&file).unwrap(), Vec::<String>::new());
        save(&file, &["theirs"]).unwrap();
    }

    #[test]
    fn a_hold_in_another_process_is_waited_for() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command, Stdio};

        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");
        let mut other = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "--nocapture"])
            .arg("git::state::tests::held_in_another_process")
            .env("WEACON_HOLD", &file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut said = BufReader::new(other.stdout.take().unwrap()).lines();
        assert!(said.any(|line| line.unwrap() == "held"));

        let (trying, tried) = mpsc::channel();
        let waiting = thread::spawn(move || {
            trying.send(()).unwrap();
            let _held = hold(&file).unwrap();
            let found = load::<String>(&file).unwrap();
            save(&file, &["ours"]).unwrap();
            found
        });
        tried.recv().unwrap();
        writeln!(other.stdin.take().unwrap(), "go").unwrap();
        // Read to its end, so what it says last never meets a closed pipe.
        said.for_each(drop);
        assert!(other.wait().unwrap().success());
        assert_eq!(waiting.join().unwrap(), ["theirs"]);
    }

    #[test]
    fn a_file_that_cant_be_read_or_parsed_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("torn.json"), "[1,").unwrap();
        fs::create_dir(dir.join("folder.json")).unwrap();

        assert_eq!(
            load::<u32>(&dir.join("torn.json")).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        load::<u32>(&dir.join("folder.json")).unwrap_err();
    }
}
