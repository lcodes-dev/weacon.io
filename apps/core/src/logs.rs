use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use weacon_observability::{Level, Sink};

use crate::platform;

pub(crate) const MAX_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const KEEP: u32 = 5;

/// A log file that moves to `name.1`, `name.2` and so on as it fills, keeping `keep` old ones.
pub(crate) struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: u32,
    file: File,
    size: u64,
}

impl RotatingFile {
    pub(crate) fn open(path: PathBuf, max_bytes: u64, keep: u32) -> io::Result<Self> {
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

/// Sends every record to `all`, and those at or above `from` to `some` as well.
pub(crate) fn tee(all: Sink, some: Sink, from: Level) -> Sink {
    Box::new(move |record| {
        all(record);
        if record.level >= from {
            some(record);
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use weacon_observability::{Logger, LoggerOptions};

    use super::*;
    use crate::test_support::{messages, recording_sink, temp_dir};

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn a_full_file_moves_aside_and_only_the_newest_are_kept() {
        let dir = temp_dir("logs-rotate");
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 10, 2).unwrap();
        for line in ["one\n", "two\n", "three\n", "four\n"] {
            file.write_all(line.as_bytes()).unwrap();
        }

        assert_eq!(read(&path), "four\n");
        assert_eq!(read(&numbered(&path, 1)), "three\n");
        assert_eq!(read(&numbered(&path, 2)), "one\ntwo\n");
        assert!(!numbered(&path, 3).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_file_counts_toward_the_limit() {
        let dir = temp_dir("logs-existing");
        let path = dir.join("weacon.log");
        fs::write(&path, "older\n").unwrap();
        let mut file = RotatingFile::open(path.clone(), 10, 1).unwrap();
        file.write_all(b"newer\n").unwrap();
        file.flush().unwrap();

        assert_eq!(read(&path), "newer\n");
        assert_eq!(read(&numbered(&path, 1)), "older\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_may_reach_the_limit_exactly() {
        let dir = temp_dir("logs-exact");
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 8, 1).unwrap();
        file.write_all(b"one\n").unwrap();
        file.write_all(b"two\n").unwrap();

        assert_eq!(MAX_BYTES, 10 << 20);
        assert_eq!(read(&path), "one\ntwo\n");
        assert!(!numbered(&path, 1).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_deleted_file_is_created_again() {
        let dir = temp_dir("logs-deleted");
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 8, 1).unwrap();
        file.write_all(b"one\n").unwrap();
        fs::remove_file(&path).unwrap();
        file.write_all(b"three\n").unwrap();

        assert_eq!(read(&path), "three\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_line_longer_than_the_limit_is_still_written() {
        let dir = temp_dir("logs-long");
        let path = dir.join("weacon.log");
        let mut file = RotatingFile::open(path.clone(), 4, 1).unwrap();
        file.write_all(b"much too long\n").unwrap();

        assert_eq!(read(&path), "much too long\n");
        assert!(!numbered(&path, 1).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tee_sends_the_quieter_records_to_one_sink_only() {
        let (all, all_seen) = recording_sink();
        let (some, some_seen) = recording_sink();
        let log = Logger::new(LoggerOptions {
            sink: tee(all, some, Level::Warn),
            now: Box::new(SystemTime::now),
            level: Level::Info,
        });
        log.info("run started", &[]);
        log.warn("sync failed", &[]);
        log.error("store full", &[]);

        assert_eq!(
            messages(&all_seen),
            ["run started", "sync failed", "store full"]
        );
        assert_eq!(messages(&some_seen), ["sync failed", "store full"]);
    }
}
