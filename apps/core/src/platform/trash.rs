//! The person's Trash: items go there instead of being deleted, and come back from it.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod macos;
#[cfg(windows)]
mod windows;

use std::fs;
use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use std::time::SystemTime;

#[cfg(target_os = "linux")]
pub(crate) use linux::trash;
#[cfg(target_os = "macos")]
pub(crate) use macos::trash;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
pub(crate) use windows::trash;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use super::Env;
use super::private_dir;

/// What to say when the system won't let an item out of the Trash.
pub(crate) const REFUSED: &str = if cfg!(target_os = "macos") {
    "macOS won't let Weacon take the worktree out of the Trash; drag it out in Finder"
} else {
    "The system won't let Weacon take the worktree out of the Trash; drag it out yourself"
};

// Tries at most this many names for an item.
const NAMES: u32 = 1000;

/// Where [`trash`] put an item, for [`restore`] to find it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Trashed {
    pub(crate) item: PathBuf,
    /// The file that tells file managers where the item came from, where the Trash keeps one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) info: Option<PathBuf>,
}

/// Nothing is trashed here, so nothing is ever deleted either.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn trash(_: &Path, _: Env<'_>, _: SystemTime) -> io::Result<Trashed> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Puts a trashed item back at `to`, which must not exist yet.
pub(crate) fn restore(trashed: &Trashed, to: &Path) -> io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    if let Some(parent) = to.parent() {
        private_dir(parent)?;
    }
    fs::rename(&trashed.item, to)?;
    if let Some(info) = &trashed.info {
        // The item is back; a stale info file only lists a ghost in the Trash.
        drop(fs::remove_file(info));
    }
    Ok(())
}

// Moves `path` to the first free name `names` gives for each try, with an info file holding
// `entry` beside it where the name has one; files are made with `open`.
fn place_first(
    path: &Path,
    entry: &[u8],
    open: &fs::OpenOptions,
    names: impl Fn(u32) -> (PathBuf, Option<PathBuf>),
) -> io::Result<Trashed> {
    for n in 0..NAMES {
        let (item, info) = names(n);
        if let Some(trashed) = place(path, item, info, entry, open)? {
            return Ok(trashed);
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

// `None` when the name is taken. The info file is made first and alone, so it claims the name;
// a name counts as free only once the system says nothing has it.
fn place(
    path: &Path,
    item: PathBuf,
    info: Option<PathBuf>,
    entry: &[u8],
    open: &fs::OpenOptions,
) -> io::Result<Option<Trashed>> {
    let file = match info.as_ref().map(|info| open.open(info)) {
        Some(Err(error)) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
        file => file.transpose()?,
    };
    let forget = || {
        if let Some(info) = &info {
            drop(fs::remove_file(info));
        }
    };
    match item.symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        found => {
            drop(file);
            forget();
            return found.map(|_| None);
        }
    }
    let written = file.map_or(Ok(()), |mut file| file.write_all(entry));
    if let Err(error) = written.and_then(|()| fs::rename(path, &item)) {
        forget();
        return Err(error);
    }
    Ok(Some(Trashed { item, info }))
}

// The highest folder above `path` still on drive `dev`.
#[cfg(unix)]
fn top_of(path: &Path, dev: u64) -> PathBuf {
    let mut top = path;
    while let Some(parent) = top.parent()
        && parent
            .symlink_metadata()
            .is_ok_and(|meta| meta.dev() == dev)
    {
        top = parent;
    }
    top.to_owned()
}

#[cfg(test)]
pub(super) fn folder(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("work.txt"), name).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::env;

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn only_an_absolute_path_that_exists_is_trashed() {
        let dir = TempDir::new().unwrap();
        let vars = env(&[]);

        let relative = trash(Path::new("work"), &vars, UNIX_EPOCH).unwrap_err();
        assert_eq!(relative.kind(), io::ErrorKind::InvalidInput);
        let missing = trash(&dir.join("missing"), &vars, UNIX_EPOCH).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_name_the_system_cant_check_is_an_error_not_a_free_one() {
        let dir = TempDir::new().unwrap();
        let item = folder(&dir, "c-a");
        fs::write(dir.join("file"), "").unwrap();

        let checked = place(
            &item,
            dir.join("file/c-a"),
            None,
            &[],
            &fs::OpenOptions::new(),
        );

        checked.unwrap_err();
        assert!(item.join("work.txt").exists());
    }

    #[test]
    fn restoring_never_replaces_what_is_there() {
        let dir = TempDir::new().unwrap();
        let trashed = Trashed {
            item: folder(&dir, "trashed"),
            info: Some(dir.join("trashed.trashinfo")),
        };
        let there = folder(&dir, "there");

        let error = restore(&trashed, &there).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(there.join("work.txt")).unwrap(), "there");
        assert!(trashed.item.exists());
    }

    #[test]
    fn a_restored_item_is_back_with_its_parents_made_and_its_info_gone() {
        let dir = TempDir::new().unwrap();
        let info = dir.join("trashed.trashinfo");
        fs::write(&info, "[Trash Info]\n").unwrap();
        let trashed = Trashed {
            item: folder(&dir, "trashed"),
            info: Some(info.clone()),
        };
        let to = dir.join("trees/w-1/c-a");

        restore(&trashed, &to).unwrap();

        assert_eq!(fs::read_to_string(to.join("work.txt")).unwrap(), "trashed");
        assert!(!trashed.item.exists());
        assert!(!info.exists());
        // An info file already gone doesn't matter.
        restore(
            &Trashed {
                item: to,
                ..trashed
            },
            &dir.join("again"),
        )
        .unwrap();
        assert!(dir.join("again/work.txt").exists());
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    #[test]
    fn elsewhere_nothing_is_trashed() {
        let dir = TempDir::new().unwrap();
        let error = trash(&folder(&dir, "c-c"), &env(&[]), UNIX_EPOCH).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(dir.join("c-c").exists());
    }
}
