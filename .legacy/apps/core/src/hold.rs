//! One hold on a lock file at a time, across this process's threads and across processes.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

use crate::platform;

// The lock files this process holds: the system's lock is the whole process's, so it can't keep
// one of its own threads from another.
static HELD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
static LET_GO: Condvar = Condvar::new();

/// A hold on a lock file, kept until dropped, so nothing else changes what it guards meanwhile.
#[derive(Debug)]
pub(crate) struct Hold {
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

/// A hold on `lock`, once another holding it, in this process or another, lets go. Its parent
/// folder is made if missing.
pub(crate) fn hold(lock: &Path) -> io::Result<Hold> {
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    while !held.insert(lock.to_path_buf()) {
        held = LET_GO.wait(held).unwrap_or_else(PoisonError::into_inner);
    }
    drop(held);
    let taken = File::create(lock).and_then(|opened| {
        platform::lock_for_process(&opened)?;
        Ok(opened)
    });
    match taken {
        Ok(opened) => Ok(Hold {
            file: Some(opened),
            lock: lock.to_path_buf(),
        }),
        Err(error) => {
            let_go(lock);
            Err(error)
        }
    }
}

fn let_go(lock: &Path) {
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(lock);
    LET_GO.notify_all();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use weacon_testing::TempDir;

    use super::*;

    // Each thread adds to a count read and written whole: with holds taking turns, none is lost.
    #[test]
    fn holds_on_one_file_take_turns() {
        let dir = TempDir::new().unwrap();
        let lock = dir.join("state/count.lock");
        let count = dir.join("state/count");
        let add = |lock: PathBuf, count: PathBuf| {
            for _ in 0..50 {
                let _held = hold(&lock).unwrap();
                let now: u32 = fs::read_to_string(&count)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(0);
                fs::write(&count, (now + 1).to_string()).unwrap();
            }
        };
        let adding: Vec<_> = (0..4)
            .map(|_| {
                let lock = lock.clone();
                let count = count.clone();
                std::thread::spawn(move || add(lock, count))
            })
            .collect();

        for thread in adding {
            thread.join().unwrap();
        }
        assert_eq!(fs::read_to_string(&count).unwrap().trim(), "200");
    }

    #[test]
    fn a_missing_parent_folder_is_made() {
        let dir = TempDir::new().unwrap();
        let _held = hold(&dir.join("made/here/one.lock")).unwrap();
        assert!(dir.join("made/here").is_dir());
    }
}
