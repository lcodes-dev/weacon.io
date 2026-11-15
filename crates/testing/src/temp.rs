use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, fs, io, process};

use crate::platform::create_private_dir;

static NEXT: AtomicU64 = AtomicU64::new(0);
const ATTEMPTS: u32 = 1000;

/// A new, empty folder of one test's own, removed when this is dropped. On Unix only this
/// user can read it.
#[derive(Debug)]
pub struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    /// # Errors
    /// When the folder can't be created.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            dir: create_new(&env::temp_dir(), &NEXT)?,
        })
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.dir
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.dir
    }
}

/// A new folder in `parent`, numbered from `next`; one already there may be someone else's,
/// so it is skipped rather than reused.
fn create_new(parent: &Path, next: &AtomicU64) -> io::Result<PathBuf> {
    for _ in 0..ATTEMPTS {
        let number = next.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("weacon-test-{}-{number}", process::id()));
        match create_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

impl Drop for TempDir {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_already_there_is_skipped() {
        let parent = TempDir::new().unwrap();
        let taken = parent.join(format!("weacon-test-{}-0", process::id()));
        fs::create_dir(&taken).unwrap();
        fs::write(taken.join("theirs"), "").unwrap();
        let dir = create_new(&parent, &AtomicU64::new(0)).unwrap();

        assert_eq!(dir, parent.join(format!("weacon-test-{}-1", process::id())));
        assert!(taken.join("theirs").exists());
        let missing = create_new(&parent.join("missing/deeper"), &AtomicU64::new(0));
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn each_test_gets_its_own_folder_removed_after_use() {
        let (one, two) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        fs::write(one.join("weacon.db"), "").unwrap();
        let dir = one.to_path_buf();

        assert_ne!(*one, *two);
        assert_eq!(one.join("weacon.db"), dir.join("weacon.db"));
        assert!(!two.join("weacon.db").exists());
        drop(one);
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_this_user_can_read_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode, 0o700);
    }
}
