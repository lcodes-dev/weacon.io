//! Watches free space on the disk holding the state folder.

use std::io;
use std::path::{Path, PathBuf};

use weacon_observability::{Field, Logger};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const LOW: u64 = 5 * GIB;
const PAUSE: u64 = GIB;
// Each level lifts only well clear of its line, so a disk at the edge doesn't flap.
const OK_AGAIN: u64 = 6 * GIB;
const RESUME: u64 = 2 * GIB;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Ok,
    Low,
    Paused,
}

impl Level {
    /// The level with `free` bytes left, coming from `self`.
    pub(crate) const fn next(self, free: u64) -> Self {
        let paused = matches!(self, Self::Paused);
        if free < PAUSE || (paused && free < RESUME) {
            Self::Paused
        } else if free < LOW || (!matches!(self, Self::Ok) && free < OK_AGAIN) {
            Self::Low
        } else {
            Self::Ok
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Low => "low",
            Self::Paused => "runs paused",
        }
    }
}

/// Reads the free bytes on the disk holding a folder; tests pass a fake.
pub(crate) type FreeSpace = fn(&Path) -> io::Result<u64>;

/// Watches one folder's disk and logs each change of level.
pub(crate) struct Watch {
    dir: PathBuf,
    free_space: FreeSpace,
    level: Level,
    failing: bool,
}

impl Watch {
    pub(crate) const fn new(dir: PathBuf, free_space: FreeSpace) -> Self {
        Self {
            dir,
            free_space,
            level: Level::Ok,
            failing: false,
        }
    }

    /// Reads the free space off the async runtime, since a slow disk can block.
    pub(crate) async fn check(&mut self, log: &Logger) {
        let (dir, free_space) = (self.dir.clone(), self.free_space);
        let free = tokio::task::spawn_blocking(move || free_space(&dir))
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)));
        self.observe(free, log);
    }

    /// A failed reading keeps the level and is logged once until one succeeds.
    fn observe(&mut self, free: io::Result<u64>, log: &Logger) {
        let free = match free {
            Ok(free) => free,
            Err(error) => {
                if !self.failing {
                    log.warn("disk check failed", &[("err", Field::error(&error))]);
                }
                self.failing = true;
                return;
            }
        };
        self.failing = false;
        let next = self.level.next(free);
        if next != self.level {
            let fields = [("free_mib", (free / MIB).into())];
            match next {
                Level::Paused => log.error("runs paused", &fields),
                Level::Low => log.warn("disk low", &fields),
                Level::Ok => log.info("disk ok", &fields),
            }
            self.level = next;
        }
    }
}

pub(crate) fn describe(free: u64) -> String {
    let level = Level::Ok.next(free).name();
    if free >= 10 * GIB {
        format!("{} GiB free ({level})", free / GIB)
    } else {
        format!("{} MiB free ({level})", free / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{messages, recording_logger};

    #[test]
    fn levels_follow_free_space_and_each_lifts_only_well_clear() {
        assert_eq!(Level::Ok.next(LOW), Level::Ok);
        assert_eq!(Level::Ok.next(LOW - 1), Level::Low);
        assert_eq!(Level::Low.next(OK_AGAIN - 1), Level::Low);
        assert_eq!(Level::Low.next(OK_AGAIN), Level::Ok);
        assert_eq!(Level::Low.next(PAUSE), Level::Low);
        assert_eq!(Level::Low.next(PAUSE - 1), Level::Paused);
        assert_eq!(Level::Paused.next(RESUME - 1), Level::Paused);
        assert_eq!(Level::Paused.next(3 * GIB / 2), Level::Paused);
        assert_eq!(Level::Low.next(11 * GIB / 2), Level::Low);
        assert_eq!(Level::Paused.next(RESUME), Level::Low);
        assert_eq!(Level::Paused.next(OK_AGAIN), Level::Ok);
    }

    #[test]
    fn each_change_is_logged_once_and_failures_once_until_a_reading_works() {
        let (log, records) = recording_logger();
        let mut watch = Watch::new(PathBuf::from("/srv"), |_| Ok(0));
        let failed = || Err(io::ErrorKind::NotFound.into());

        let readings = [
            Ok(LOW),
            Ok(GIB / 2),
            Ok(GIB / 2),
            failed(),
            failed(),
            Ok(3 * GIB),
            failed(),
            Ok(OK_AGAIN),
        ];
        for free in readings {
            watch.observe(free, &log);
        }

        assert_eq!(
            messages(&records),
            [
                "runs paused",
                "disk check failed",
                "disk low",
                "disk check failed",
                "disk ok"
            ]
        );
        let first = records.lock().unwrap().first().cloned().unwrap();
        assert!(format!("{:?}", first.fields).contains("512"));
    }

    #[test]
    fn free_space_reads_in_mib_or_gib() {
        assert_eq!(describe(12 * GIB + MIB), "12 GiB free (ok)");
        assert_eq!(describe(10 * GIB), "10 GiB free (ok)");
        assert_eq!(describe(3 * GIB), "3072 MiB free (low)");
        assert_eq!(describe(MIB), "1 MiB free (runs paused)");
    }
}
