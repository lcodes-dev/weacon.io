//! The Trash, as macOS's Finder keeps it: the item alone, with no record beside it.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::SystemTime;

#[cfg(target_os = "macos")]
use super::{Trashed, place_first, top_of};
use crate::platform::private_dir;
#[cfg(target_os = "macos")]
use crate::platform::{Env, home_dir};

/// Moves `path`, an absolute path, to the Trash on its own drive. Fails, leaving `path` alone,
/// where the drive has none Finder made.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "getuid has no safe API in std")]
pub(crate) fn trash(path: &Path, env: Env<'_>, _: SystemTime) -> io::Result<Trashed> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let dev = path.symlink_metadata()?.dev();
    let parent = path
        .parent()
        .ok_or(io::ErrorKind::InvalidInput)?
        .canonicalize()?;
    let home = home_dir(env).ok_or(io::ErrorKind::NotFound)?;
    let home_dev = fs::metadata(&home)?.dev();
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    let dir = trash_dir(&home, home_dev, &top_of(&parent, dev), dev, uid)?;
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    place_first(path, &[], &fs::OpenOptions::new(), |n| {
        (dir.join(numbered(name, n)), None)
    })
}

/// The Trash for an item on drive `dev`: the home one on the home's drive, else `uid`'s in the
/// drive's `.Trashes`, which is never made where Finder hasn't made it.
pub(super) fn trash_dir(
    home: &Path,
    home_dev: u64,
    top: &Path,
    dev: u64,
    uid: u32,
) -> io::Result<PathBuf> {
    let dir = if dev == home_dev {
        home.join(".Trash")
    } else {
        let shared = top.join(".Trashes");
        if !shared.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let own = shared.join(uid.to_string());
        private_dir(&own)?;
        // Anyone can make a folder in `.Trashes`, so it must be this user's alone.
        let meta = own.symlink_metadata()?;
        if meta.uid() != uid || meta.mode() & 0o022 != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        own
    };
    let meta = dir.symlink_metadata()?;
    if meta.is_dir() && meta.dev() == dev {
        Ok(dir)
    } else {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// `name` for the `n`th try, numbered as Finder does from the second: `site 2.txt`.
pub(super) fn numbered(name: &OsStr, n: u32) -> OsString {
    if n == 0 {
        return name.to_owned();
    }
    let name = Path::new(name);
    let mut numbered = name.file_stem().unwrap_or_default().to_owned();
    numbered.push(format!(" {}", n + 1));
    if let Some(ending) = name.extension() {
        numbered.push(".");
        numbered.push(ending);
    }
    numbered
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn items_on_the_homes_drive_go_to_the_home_trash() {
        let dir = TempDir::new().unwrap();
        let dev = fs::metadata(&dir).unwrap().dev();
        fs::create_dir_all(dir.join(".Trash")).unwrap();

        let picked = trash_dir(&dir, dev, &dir.join("top"), dev, 501).unwrap();

        assert_eq!(picked, dir.join(".Trash"));
        // Without the home Trash, nothing is made in its place.
        fs::remove_dir(dir.join(".Trash")).unwrap();
        let error = trash_dir(&dir, dev, &dir.join("top"), dev, 501).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!dir.join(".Trash").exists());
    }

    #[test]
    fn items_on_another_drive_go_to_its_trashes_once_finder_made_them() {
        let dir = TempDir::new().unwrap();
        let dev = fs::metadata(&dir).unwrap().dev();
        let home = dir.join("home");
        let top = dir.join("volume");
        fs::create_dir_all(&top).unwrap();

        let uid = fs::metadata(&dir).unwrap().uid();
        let missing = trash_dir(&home, dev + 1, &top, dev, uid).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::Unsupported);
        assert!(!top.join(".Trashes").exists());

        fs::create_dir_all(top.join(".Trashes")).unwrap();
        let picked = trash_dir(&home, dev + 1, &top, dev, uid).unwrap();
        assert_eq!(picked, top.join(".Trashes").join(uid.to_string()));
        let elsewhere = trash_dir(&home, dev + 1, &top, dev + 2, uid).unwrap_err();
        assert_eq!(elsewhere.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn a_trashes_folder_another_user_owns_or_can_change_is_never_used() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let meta = fs::metadata(&dir).unwrap();
        let (dev, uid) = (meta.dev(), meta.uid());
        let top = dir.join("volume");
        let own = top.join(".Trashes").join(uid.to_string());
        fs::create_dir_all(&own).unwrap();
        let refused = |uid| {
            let error = trash_dir(&dir.join("home"), dev + 1, &top, dev, uid).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        };

        fs::set_permissions(&own, fs::Permissions::from_mode(0o777)).unwrap();
        refused(uid);
        fs::set_permissions(&own, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(top.join(".Trashes").join((uid + 1).to_string())).unwrap();
        refused(uid + 1);
    }

    #[test]
    fn a_taken_name_is_numbered_as_finder_does() {
        let names: Vec<OsString> = (0..3).map(|n| numbered("c-a".as_ref(), n)).collect();
        assert_eq!(names, ["c-a", "c-a 2", "c-a 3"]);
        assert_eq!(numbered("site.v2".as_ref(), 1), "site 2.v2");
    }
}
