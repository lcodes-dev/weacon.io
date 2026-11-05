use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, fs, io, process};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ATTEMPTS: u32 = 1000;

/// A new, empty folder of one test's own, removed when this is dropped.
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

    /// The path of `name` in this folder, such as a database file.
    #[must_use]
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A new folder in `parent`, numbered from `next`; one already there may be someone else's,
/// so it is skipped rather than reused.
fn create_new(parent: &Path, next: &AtomicU64) -> io::Result<PathBuf> {
    for _ in 0..ATTEMPTS {
        let number = next.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("weacon-test-{}-{number}", process::id()));
        match fs::create_dir(&dir) {
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
        let taken = parent.file(&format!("weacon-test-{}-0", process::id()));
        fs::create_dir(&taken).unwrap();
        fs::write(taken.join("theirs"), "").unwrap();
        let dir = create_new(parent.dir(), &AtomicU64::new(0)).unwrap();

        assert_eq!(
            dir,
            parent.file(&format!("weacon-test-{}-1", process::id()))
        );
        assert!(taken.join("theirs").exists());
        let missing = create_new(&parent.file("missing/deeper"), &AtomicU64::new(0));
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn each_test_gets_its_own_folder_removed_after_use() {
        let (one, two) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        fs::write(one.file("weacon.db"), "").unwrap();
        let dir = one.dir().to_owned();

        assert_ne!(one.dir(), two.dir());
        assert_eq!(one.file("weacon.db"), dir.join("weacon.db"));
        assert!(!two.file("weacon.db").exists());
        drop(one);
        assert!(!dir.exists());
    }
}
