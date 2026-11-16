//! A plain file of a folder, opened only when it isn't a link.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// The file at `relative`, a `/`-separated path, in `root`, opened to read; a link at any
/// step below `root`, a `..`, a folder, a pipe and a path that isn't there are refused.
pub(crate) fn open_beneath(root: &Path, relative: &str) -> io::Result<File> {
    let mut path = root.to_path_buf();
    let mut plain = false;
    for part in relative.split('/') {
        path.push(part);
        let about = fs::symlink_metadata(&path)?;
        if part == ".." || about.is_symlink() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        plain = about.is_file();
    }
    if !plain {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    open_unfollowed(&path)
}

// A link put there since the check is refused too, where the system can say so.
#[cfg(unix)]
fn open_unfollowed(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_unfollowed(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use weacon_testing::TempDir;

    use super::*;

    fn read(root: &Path, relative: &str) -> Option<String> {
        let mut text = String::new();
        open_beneath(root, relative)
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    }

    #[test]
    fn opens_a_plain_file_and_nothing_else() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/c.md"), "inside").unwrap();

        assert_eq!(read(&dir, "a/c.md").as_deref(), Some("inside"));
        assert_eq!(read(&dir, "a"), None);
        assert_eq!(read(&dir, "a/x.md"), None);
        assert_eq!(read(&dir.join("a"), "../a/c.md"), None);
        let linked = crate::platform::plant_link(&dir.join("a/c.md"), &dir.join("link.md"))
            && crate::platform::plant_link(&dir.join("a"), &dir.join("folder"));
        if linked {
            assert_eq!(read(&dir, "link.md"), None);
            assert_eq!(read(&dir, "folder/c.md"), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn opens_no_pipe() {
        let dir = TempDir::new().unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("pipe.md"))
            .status()
            .unwrap();
        assert!(made.success());

        open_beneath(&dir, "pipe.md").unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn the_open_itself_follows_no_link() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("c.md"), "inside").unwrap();
        std::os::unix::fs::symlink(dir.join("c.md"), dir.join("link.md")).unwrap();

        open_unfollowed(&dir.join("c.md")).unwrap();
        open_unfollowed(&dir.join("link.md")).unwrap_err();
    }
}
