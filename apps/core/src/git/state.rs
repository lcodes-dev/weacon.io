//! The git module's own state files: each a JSON list, read whole and written whole, in the
//! workspace's folder.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;
use weacon_observability::Fault;
use weacon_protocol::{RepoId, WorkspaceId, code};

use super::Git;
use super::chats::ChatRepos;
use super::removal::Journal;
use super::repos::{Repo, Repos};
use crate::platform;
use crate::workspaces::Workspaces;

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

// The lock files this process holds: the system's lock is the whole process's, so it can't keep
// one of its own threads from another.
static HELD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// A hold on changing a file, kept until dropped, so nothing else changes it meanwhile.
#[derive(Debug)]
pub(super) struct Hold {
    file: Option<File>,
    lock: PathBuf,
}

impl Drop for Hold {
    fn drop(&mut self) {
        // Closed first: closing the file lets the system's lock go, and another thread could
        // otherwise take the path and then lose its lock to this close.
        drop(self.file.take());
        let_go(&self.lock);
    }
}

/// A hold on changing `file`. While another holds it, in this process or another, this fails as
/// `WouldBlock`.
pub(super) fn hold(file: &Path) -> io::Result<Hold> {
    let lock = file.with_extension("lock");
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent)?;
    }
    if !HELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(lock.clone())
    {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let taken = File::create(&lock).and_then(|opened| {
        platform::lock_for_process(&opened)?;
        Ok(opened)
    });
    match taken {
        Ok(opened) => Ok(Hold {
            file: Some(opened),
            lock,
        }),
        Err(error) => {
            let_go(&lock);
            Err(error)
        }
    }
}

fn let_go(lock: &Path) {
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(lock);
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn a_list_comes_back_as_saved_and_none_is_empty() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");

        assert_eq!(load::<u32>(&file).unwrap(), Vec::<u32>::new());
        save(&file, &[3_u32, 1]).unwrap();
        assert_eq!(load::<u32>(&file).unwrap(), [3, 1]);
    }

    #[test]
    fn a_file_is_held_by_one_at_a_time() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");

        let held = hold(&file).unwrap();
        assert_eq!(hold(&file).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        drop(held);
        hold(&file).unwrap();
    }

    // Run by the test below, in a process of its own: holds the file it's given until told.
    #[test]
    #[ignore = "a helper the test below runs in another process"]
    fn held_in_another_process() {
        use std::io::{BufRead, Write};

        let Some(file) = std::env::var_os("WEACON_HOLD") else {
            return;
        };
        let _held = hold(Path::new(&file)).unwrap();
        writeln!(io::stdout(), "held").unwrap();
        io::stdout().flush().unwrap();
        io::stdin().lock().read_line(&mut String::new()).unwrap();
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

        assert_eq!(hold(&file).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        writeln!(other.stdin.take().unwrap(), "go").unwrap();
        // Read to its end, so what it says last never meets a closed pipe.
        said.for_each(drop);
        assert!(other.wait().unwrap().success());
        hold(&file).unwrap();
    }

    // Each start forks this process, and a child holds copies of its open files until it runs
    // its program: a hold let go must be free at once even so.
    #[test]
    fn a_hold_let_go_is_free_at_once_while_programs_start() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");
        let started = Arc::new(AtomicUsize::new(0));
        // A working folder makes each start a fork, whose child keeps the copies longest.
        let start = |counted: Arc<AtomicUsize>, here: PathBuf| {
            while counted.load(Ordering::SeqCst) < 400 {
                let mut git = std::process::Command::new("git");
                drop(git.arg("--version").current_dir(&here).output());
                counted.fetch_add(1, Ordering::SeqCst);
            }
        };
        let starting: Vec<_> = (0..4)
            .map(|_| {
                let (counted, here) = (Arc::clone(&started), dir.to_path_buf());
                std::thread::spawn(move || start(counted, here))
            })
            .collect();

        let mut taken = true;
        while started.load(Ordering::SeqCst) < 400 {
            taken &= hold(&file).is_ok();
        }

        for thread in starting {
            thread.join().unwrap();
        }
        assert!(taken);
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
