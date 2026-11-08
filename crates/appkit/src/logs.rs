use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::platform;

/// How large a log file grows before it moves aside.
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many moved-aside log files are kept.
pub const KEEP: u32 = 5;

/// A log file that moves to `name.1`, `name.2` and so on as it fills, keeping `keep` old ones.
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: u32,
    file: File,
    size: u64,
}

impl RotatingFile {
    /// Opens `path` for appending, creating it readable only by this user.
    ///
    /// # Errors
    /// When the file can't be opened or read.
    pub fn open(path: PathBuf, max_bytes: u64, keep: u32) -> io::Result<Self> {
        let file = platform::private_append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            max_bytes,
            keep,
            file,
            size,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        for older in (1..self.keep).rev() {
            if let Err(error) =
                fs::rename(numbered(&self.path, older), numbered(&self.path, older + 1))
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(error);
            }
        }
        if let Err(error) = fs::rename(&self.path, numbered(&self.path, 1))
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        self.file = platform::private_append(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if self.size > 0 && self.size.saturating_add(len) > self.max_bytes {
            self.rotate()?;
        }
        let written = self.file.write(buf)?;
        self.size += u64::try_from(written).unwrap_or(u64::MAX);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn numbered(path: &Path, number: u32) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(format!(".{number}"));
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use weacon_testing::TempDir;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn a_full_file_moves_aside_and_only_the_newest_are_kept() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 10, 2).unwrap();
        for line in ["one\n", "two\n", "three\n", "four\n"] {
            file.write_all(line.as_bytes()).unwrap();
        }

        assert_eq!(read(&path), "four\n");
        assert_eq!(read(&numbered(&path, 1)), "three\n");
        assert_eq!(read(&numbered(&path, 2)), "one\ntwo\n");
        assert!(!numbered(&path, 3).exists());
    }

    #[test]
    fn an_existing_file_counts_toward_the_limit() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.log");
        fs::write(&path, "older\n").unwrap();
        let mut file = RotatingFile::open(path.clone(), 10, 1).unwrap();
        file.write_all(b"newer\n").unwrap();
        file.flush().unwrap();

        assert_eq!(read(&path), "newer\n");
        assert_eq!(read(&numbered(&path, 1)), "older\n");
    }

    #[test]
    fn a_file_may_reach_the_limit_exactly() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 8, 1).unwrap();
        file.write_all(b"one\n").unwrap();
        file.write_all(b"two\n").unwrap();

        assert_eq!(MAX_BYTES, 10 << 20);
        assert_eq!(read(&path), "one\ntwo\n");
        assert!(!numbered(&path, 1).exists());
    }

    #[test]
    fn a_deleted_file_is_created_again() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 8, 1).unwrap();
        file.write_all(b"one\n").unwrap();
        fs::remove_file(&path).unwrap();
        file.write_all(b"three\n").unwrap();

        assert_eq!(read(&path), "three\n");
    }

    #[test]
    fn a_line_longer_than_the_limit_is_still_written() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 4, 1).unwrap();
        file.write_all(b"much too long\n").unwrap();

        assert_eq!(read(&path), "much too long\n");
        assert!(!numbered(&path, 1).exists());
    }
}
