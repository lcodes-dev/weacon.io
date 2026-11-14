use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use weacon_protocol::{Pick, RequestId};

use crate::platform;

/// Elements picked in Chrome, one file per request.
#[derive(Clone, Debug)]
pub(crate) struct Picks {
    dir: PathBuf,
}

#[derive(Debug)]
pub(crate) enum KeepError {
    /// The request was kept before with other content.
    Conflict,
    Io(io::Error),
}

impl Picks {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("picks"),
        }
    }

    /// Keeps `pick`, or does nothing when its request was kept before with the same content.
    pub(crate) fn keep(&self, pick: &Pick) -> Result<(), KeepError> {
        let json = serde_json::to_vec(pick).map_err(|error| KeepError::Io(error.into()))?;
        // Read back whatever was published: this pick, or one kept before it.
        let published = self.publish(&pick.request, &json);
        match self.read(&pick.request).map_err(KeepError::Io)? {
            Some(kept) if kept == *pick => Ok(()),
            Some(_) => Err(KeepError::Conflict),
            None => {
                Err(KeepError::Io(published.err().unwrap_or_else(|| {
                    io::Error::from(io::ErrorKind::NotFound)
                })))
            }
        }
    }

    /// The pick kept for `request`, if any.
    pub(crate) fn read(&self, request: &RequestId) -> io::Result<Option<Pick>> {
        match fs::read(self.path(request)) {
            Ok(json) => Ok(Some(serde_json::from_slice(&json)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    // Written aside under a name of its own, then linked into place: a reader never sees half a
    // pick, and of two keeps of one request at once, only one is published.
    fn publish(&self, request: &RequestId, json: &[u8]) -> io::Result<()> {
        platform::private_dir(&self.dir)?;
        let partial = self.dir.join(format!(
            "{}.{}-{}.partial",
            name(request),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let published =
            write_new(&partial, json).and_then(|()| fs::hard_link(&partial, self.path(request)));
        // Linked or not, the partial name is no longer needed.
        drop(fs::remove_file(&partial));
        published
    }

    fn path(&self, request: &RequestId) -> PathBuf {
        self.dir.join(name(request) + ".json")
    }
}

fn name(request: &RequestId) -> String {
    platform::case_safe_name("pick-", request.as_str())
}

static NEXT: AtomicU64 = AtomicU64::new(0);

fn write_new(path: &Path, json: &[u8]) -> io::Result<()> {
    // A partial left by a crashed run under a reused process id is stale.
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    let mut file = weacon_appkit::private_append(path)?;
    file.write_all(json)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::pick;

    #[test]
    fn a_pick_is_kept_once_and_read_back() {
        let dir = TempDir::new().unwrap();
        let picks = Picks::new(&dir);
        let first = pick("p-1", "<button>Save</button>");

        assert_eq!(picks.read(&first.request).unwrap(), None);
        picks.keep(&first).unwrap();
        picks.keep(&first).unwrap();

        assert_eq!(picks.read(&first.request).unwrap(), Some(first));
        assert!(matches!(
            picks.keep(&pick("p-1", "<button>Other</button>")),
            Err(KeepError::Conflict)
        ));
        assert_eq!(
            fs::read_dir(dir.join("picks")).unwrap().count(),
            1,
            "only the kept file remains"
        );
    }

    #[test]
    fn of_different_picks_kept_at_once_under_one_request_exactly_one_wins() {
        let dir = TempDir::new().unwrap();
        let picks = Picks::new(&dir);
        let tries: Vec<_> = (0..8)
            .map(|n| pick("p-2", &format!("<p>{n}</p>")))
            .collect();
        let results: Vec<_> = std::thread::scope(|scope| {
            #[expect(
                clippy::needless_collect,
                reason = "every thread starts before any is joined"
            )]
            let keeps: Vec<_> = tries
                .iter()
                .map(|each| scope.spawn(|| picks.keep(each)))
                .collect();
            keeps.into_iter().map(|keep| keep.join().unwrap()).collect()
        });
        let won: Vec<_> = tries
            .iter()
            .zip(&results)
            .filter(|(_, result)| result.is_ok())
            .map(|(each, _)| each)
            .collect();

        assert_eq!(won.len(), 1, "{results:?}");
        assert!(
            results
                .iter()
                .filter(|result| result.is_err())
                .all(|result| matches!(result, Err(KeepError::Conflict)))
        );
        assert_eq!(picks.read(&won[0].request).unwrap().as_ref(), Some(won[0]));
        assert_eq!(fs::read_dir(dir.join("picks")).unwrap().count(), 1);
    }

    #[test]
    fn requests_differing_only_in_case_keep_their_own_files() {
        let dir = TempDir::new().unwrap();
        let picks = Picks::new(&dir);
        let upper = pick("p-Ab", "<p>upper</p>");
        let lower = pick("p-ab", "<p>lower</p>");
        picks.keep(&upper).unwrap();
        picks.keep(&lower).unwrap();

        assert_eq!(picks.read(&upper.request).unwrap(), Some(upper));
        assert_eq!(picks.read(&lower.request).unwrap(), Some(lower));
        assert!(dir.join("picks/pick-p-.ab.json").exists());
        assert!(dir.join("picks/pick-p-ab.json").exists());
    }

    #[test]
    fn a_pick_that_cant_be_written_or_read_fails_with_io() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("not-a-folder");
        fs::write(&file, "").unwrap();
        let blocked = pick("p-3", "<p/>");
        // A folder where the pick's file belongs can be neither replaced nor read.
        let unreadable = Picks::new(&dir);
        fs::create_dir_all(dir.join("picks/pick-p-3.json")).unwrap();

        assert!(matches!(
            Picks::new(&file).keep(&blocked),
            Err(KeepError::Io(_))
        ));
        assert!(matches!(unreadable.keep(&blocked), Err(KeepError::Io(_))));
        unreadable.read(&blocked.request).unwrap_err();
    }
}
