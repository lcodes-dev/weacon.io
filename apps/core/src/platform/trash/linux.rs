//! The freedesktop.org Trash, as Linux desktops keep it.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Trashed, place_first, top_of};
use crate::platform::{Env, private_dir, xdg_dir};

/// Moves `path`, an absolute path, to the Trash on its own drive: the home one, or the one at
/// the drive's top. Fails, leaving `path` alone, where the drive has none.
pub(crate) fn trash(path: &Path, env: Env<'_>, now: SystemTime) -> io::Result<Trashed> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let dev = path.symlink_metadata()?.dev();
    // Resolved, so the walk to the drive's top never stops at a link to another drive.
    let parent = path
        .parent()
        .ok_or(io::ErrorKind::InvalidInput)?
        .canonicalize()?;
    let home = xdg_dir(env, "XDG_DATA_HOME", ".local/share").map(|dir| dir.join("Trash"));
    let places = Places {
        home,
        top: top_of(&parent, dev),
        dev,
        uid: own_uid(),
    };
    trash_into(path, &parent, &places, now)
}

// Where an item's Trash may be.
struct Places {
    home: Option<PathBuf>,
    // The item's drive, and its top.
    top: PathBuf,
    dev: u64,
    uid: u32,
}

// `trash` once it knows the item's resolved `parent` and its Trash's `places`.
fn trash_into(path: &Path, parent: &Path, places: &Places, now: SystemTime) -> io::Result<Trashed> {
    let dir = places.pick().ok_or(io::ErrorKind::Unsupported)?;
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let full = parent.join(name);
    // A drive's own Trash says where from its top, so the record holds wherever it's mounted.
    let from = if Some(&dir) == places.home.as_ref() {
        full.as_path()
    } else {
        full.strip_prefix(&places.top).unwrap_or(&full)
    };
    let entry = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode(from.as_os_str().as_bytes()),
        local_time(now)?
    );
    let mut open = fs::OpenOptions::new();
    open.write(true).create_new(true).mode(0o600);
    place_first(path, entry.as_bytes(), &open, |n| {
        let mut stem = name.to_owned();
        if n > 0 {
            stem.push(format!(".{}", n + 1));
        }
        let item = dir.join("files").join(&stem);
        stem.push(".trashinfo");
        (item, Some(dir.join("info").join(stem)))
    })
}

#[expect(unsafe_code, reason = "getuid has no safe API in std")]
fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and can't fail.
    unsafe { libc::getuid() }
}

fn usable_home(home: PathBuf, places: &Places) -> Option<PathBuf> {
    usable(home, places.uid, places.dev, 0o022)
}

impl Places {
    // The home Trash if it's on the item's drive, else the shared one at the drive's top if its
    // sticky bit is set, else this user's own there; each with `files` and `info` made.
    fn pick(&self) -> Option<PathBuf> {
        let shared = self.top.join(".Trash");
        let sticky = shared
            .symlink_metadata()
            .is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & libc::S_ISVTX != 0);
        // A drive's Trash, unlike the home one, may sit where others can look, so it's closed.
        let usable = |dir: PathBuf| usable(dir, self.uid, self.dev, 0o077);
        self.home
            .clone()
            .and_then(|home| usable_home(home, self))
            .or_else(|| {
                sticky
                    .then(|| usable(shared.join(self.uid.to_string())))
                    .flatten()
            })
            .or_else(|| usable(self.top.join(format!(".Trash-{}", self.uid))))
    }
}

// `dir` as a Trash, when it and its `files` and `info` can be made and are real folders on
// drive `dev` that only `uid` owns and can change, and `dir` has none of the `closed` bits.
fn usable(dir: PathBuf, uid: u32, dev: u64, closed: u32) -> Option<PathBuf> {
    let own = |path: &Path, closed: u32| {
        path.symlink_metadata().is_ok_and(|meta| {
            meta.is_dir() && meta.dev() == dev && meta.uid() == uid && meta.mode() & closed == 0
        })
    };
    let parts = [dir.join("files"), dir.join("info")];
    let made = parts.iter().all(|part| private_dir(part).is_ok());
    (made && own(&dir, closed) && parts.iter().all(|part| own(part, 0o022))).then_some(dir)
}

// A path in a `.trashinfo` file: bytes other than unreserved ones and `/` as `%XX`.
pub(super) fn encode(path: &[u8]) -> String {
    let mut encoded = String::new();
    for &byte in path {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            // Writing to a String can't fail.
            write!(encoded, "%{byte:02X}").unwrap_or_default();
        }
    }
    encoded
}

// `now` as the local `YYYY-MM-DDThh:mm:ss` the spec asks for.
#[expect(unsafe_code, reason = "localtime_r has no safe API in std")]
pub(super) fn local_time(now: SystemTime) -> io::Result<String> {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let time = libc::time_t::try_from(secs).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut parts = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: `time` and `parts` are valid for the call, which writes only `parts`.
    if unsafe { libc::localtime_r(&raw const time, parts.as_mut_ptr()) }.is_null() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: localtime_r returned non-null, so it filled `parts`.
    let tm = unsafe { parts.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        i64::from(tm.tm_year) + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use weacon_testing::TempDir;

    use super::*;
    use crate::platform::trash::{folder, restore};

    fn pick(home: Option<PathBuf>, top: &Path, uid: u32, dev: u64) -> Option<PathBuf> {
        let top = top.to_owned();
        Places {
            home,
            top,
            dev,
            uid,
        }
        .pick()
    }

    // `trash`, with the drive's top in the test's own folder, so nothing reaches the system's.
    fn trash(path: &Path, data: &Path, now: SystemTime) -> io::Result<Trashed> {
        let parent = path.parent().unwrap().canonicalize().unwrap();
        let uid = fs::metadata(&parent).unwrap().uid();
        let top = data.parent().unwrap().join("top");
        let dev = fs::metadata(path).unwrap().dev();
        let places = Places {
            home: Some(data.join("Trash")),
            top,
            dev,
            uid,
        };
        trash_into(path, &parent, &places, now)
    }

    #[test]
    fn on_linux_items_go_to_the_home_trash_with_an_info_file_each() {
        let dir = TempDir::new().unwrap();
        let data = dir.join("data");
        let first = folder(&dir, "c a");
        let second_dir = dir.join("other");
        let second = folder(&second_dir, "c a");

        let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let one = trash(&first, &data, when).unwrap();
        let two = trash(&second, &data, when).unwrap();

        assert!(!first.exists() && !second.exists());
        assert_eq!(one.item, data.join("Trash/files/c a"));
        assert_eq!(two.item, data.join("Trash/files/c a.2"));
        assert_eq!(
            fs::read_to_string(one.item.join("work.txt")).unwrap(),
            "c a"
        );
        let info = fs::read_to_string(one.info.as_ref().unwrap()).unwrap();
        let path = encode(first.as_os_str().as_encoded_bytes());
        let deleted = local_time(when).unwrap();
        assert_eq!(
            info,
            format!("[Trash Info]\nPath={path}\nDeletionDate={deleted}\n")
        );
        assert_eq!(two.info, Some(data.join("Trash/info/c a.2.trashinfo")));

        restore(&one, &first).unwrap();
        assert_eq!(fs::read_to_string(first.join("work.txt")).unwrap(), "c a");
    }

    #[test]
    fn on_linux_a_name_taken_in_the_trash_is_skipped() {
        let dir = TempDir::new().unwrap();
        let data = dir.join("data");
        // An item left without its info file still holds its name.
        private_dir(&data.join("Trash/files")).unwrap();
        folder(&data.join("Trash/files"), "c-b");

        let trashed = trash(&folder(&dir, "c-b"), &data, UNIX_EPOCH).unwrap();

        assert_eq!(trashed.item, data.join("Trash/files/c-b.2"));
        assert!(!data.join("Trash/info/c-b.trashinfo").exists());
    }

    #[test]
    fn on_linux_a_drives_own_trash_records_where_from_its_top() {
        let dir = TempDir::new().unwrap();
        let item = folder(&dir.join("work"), "c-d");
        let parent = dir.join("work").canonicalize().unwrap();
        let meta = fs::metadata(&parent).unwrap();
        let places = Places {
            home: None,
            top: parent.parent().unwrap().to_owned(),
            dev: meta.dev(),
            uid: meta.uid(),
        };

        let trashed = trash_into(&item, &parent, &places, UNIX_EPOCH).unwrap();

        let info = fs::read_to_string(trashed.info.unwrap()).unwrap();
        assert!(info.contains("\nPath=work/c-d\n"), "{info}");
    }

    #[test]
    fn on_linux_paths_are_percent_encoded_and_times_are_local() {
        assert_eq!(encode(b"/a b/c%d/\xc3\xa9-._~"), "/a%20b/c%25d/%C3%A9-._~");
        let date = local_time(UNIX_EPOCH).unwrap();
        let shape: String = date
            .chars()
            .map(|letter| if letter.is_ascii_digit() { '9' } else { letter })
            .collect();
        assert_eq!(shape, "9999-99-99T99:99:99");
        assert!(
            date.starts_with("1969-12-31T") || date.starts_with("1970-01-01T"),
            "{date}"
        );
    }

    #[test]
    fn on_linux_another_drive_uses_the_trash_at_its_top() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = TempDir::new().unwrap();
        let meta = fs::metadata(&dir).unwrap();
        let (dev, uid) = (meta.dev(), meta.uid());
        let own_trash = format!(".Trash-{uid}");
        fs::write(dir.join("file"), "").unwrap();
        // A home Trash that can't be made, as on another drive.
        let home = Some(dir.join("file/Trash"));
        let pick = |top: &Path, uid, dev| pick(home.clone(), top, uid, dev);
        let picked = |top: &Path| pick(top, uid, dev).unwrap();

        let own = dir.join("own");
        assert_eq!(picked(&own), own.join(&own_trash));
        assert!(own.join(&own_trash).join("files").is_dir());
        assert!(own.join(&own_trash).join("info").is_dir());

        let shared = dir.join("shared");
        fs::create_dir_all(shared.join(".Trash")).unwrap();
        assert_eq!(picked(&shared), shared.join(&own_trash));
        fs::set_permissions(shared.join(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(picked(&shared), shared.join(".Trash").join(uid.to_string()));

        assert_eq!(pick(&dir.join("elsewhere"), uid, dev + 1), None);
        let home_trash = self::pick(Some(dir.join("data/Trash")), &own, uid, dev);
        assert_eq!(home_trash, Some(dir.join("data/Trash")));
    }

    #[test]
    fn on_linux_a_trash_another_user_owns_or_can_change_is_never_used() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = TempDir::new().unwrap();
        let meta = fs::metadata(&dir).unwrap();
        let (dev, uid) = (meta.dev(), meta.uid());
        let home = dir.join("Trash");
        assert_eq!(pick(Some(home.clone()), &dir, uid, dev), Some(home.clone()));

        // Folders this user doesn't own.
        let top = dir.join("top");
        assert_eq!(pick(Some(home.clone()), &top, uid + 1, dev), None);
        // A home Trash others can look into is still this user's own; a drive's isn't.
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(pick(Some(home.clone()), &top, uid, dev), Some(home.clone()));
        let drives = top.join(format!(".Trash-{uid}"));
        assert_eq!(pick(None, &top, uid, dev), Some(drives.clone()));
        fs::set_permissions(&drives, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(pick(None, &top, uid, dev), None);
        fs::set_permissions(&drives, fs::Permissions::from_mode(0o700)).unwrap();
        // A folder others can write to, or a link in place of one.
        fs::set_permissions(home.join("files"), fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            pick(Some(home.clone()), &top, uid, dev),
            Some(top.join(format!(".Trash-{uid}")))
        );
        fs::set_permissions(home.join("files"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir(home.join("info")).unwrap();
        std::os::unix::fs::symlink(&dir, home.join("info")).unwrap();
        assert_ne!(pick(Some(home.clone()), &top, uid, dev), Some(home));
    }

    #[test]
    fn on_linux_an_item_that_cant_be_moved_stays_and_leaves_no_info_file() {
        let dir = TempDir::new().unwrap();
        let item = folder(&dir, "c-x");
        // The Trash inside the item itself, which can't move into itself.
        let data = item.join("data");

        let error = trash(&item, &data, UNIX_EPOCH).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(item.join("work.txt").exists());
        assert_eq!(fs::read_dir(data.join("Trash/info")).unwrap().count(), 0);
    }

    #[test]
    fn on_linux_a_name_too_long_for_the_trash_fails_and_stays() {
        let dir = TempDir::new().unwrap();
        let data = dir.join("data");
        let item = folder(&dir, &"n".repeat(250));

        let error = trash(&item, &data, UNIX_EPOCH).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidFilename);
        assert!(item.join("work.txt").exists());
    }

    #[test]
    fn on_linux_the_top_of_a_drive_is_its_highest_folder() {
        use std::os::unix::fs::MetadataExt;

        let dir = TempDir::new().unwrap();
        let path = dir.join("a/b");
        fs::create_dir_all(&path).unwrap();
        let dev = fs::metadata(&path).unwrap().dev();

        let top = top_of(&path, dev);

        assert!(path.starts_with(&top));
        assert_eq!(fs::metadata(&top).unwrap().dev(), dev);
        if let Some(parent) = top.parent() {
            assert_ne!(fs::metadata(parent).unwrap().dev(), dev);
        }
        assert_eq!(top_of(&path, dev + 1), path);
    }
}
