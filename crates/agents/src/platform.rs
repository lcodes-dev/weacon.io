//! What differs between operating systems.

mod pty;

pub(crate) use pty::Pty;
#[cfg(unix)]
pub(crate) use unix::links;
#[cfg(unix)]
use unix::{links_of, open_unfollowed};
#[cfg(windows)]
pub(crate) use windows::links;
#[cfg(windows)]
use windows::{links_of, open_unfollowed};

use std::fs::File;
use std::io;
use std::path::Path;

/// `file` opened to read, when it is a regular file with one link and not a symlink.
///
/// # Errors
///
/// When `file` can't be opened, or is anything else.
pub fn open_plain(file: &Path) -> io::Result<File> {
    let opened = open_unfollowed(file)?;
    if opened.metadata()?.is_file() && links_of(&opened)? == 1 {
        Ok(opened)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a plain file",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Path;

    /// How many hard links `file` has.
    pub(crate) fn links(file: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(file)?.nlink())
    }

    /// `file` opened to read, a symlink refused. Non-blocking, so a pipe can't hold the open.
    pub(super) fn open_unfollowed(file: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(file)
    }

    pub(super) fn links_of(opened: &File) -> io::Result<u64> {
        Ok(opened.metadata()?.nlink())
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
    };

    /// How many hard links `file` has.
    pub(crate) fn links(file: &Path) -> io::Result<u64> {
        links_of(&File::open(file)?)
    }

    /// `file` opened to read; a link is opened itself, never its target.
    pub(super) fn open_unfollowed(file: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(file)
    }

    #[expect(
        unsafe_code,
        reason = "std has no stable way to count a file's links on Windows"
    )]
    pub(super) fn links_of(opened: &File) -> io::Result<u64> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle stays open for the call, and `info` is room for one structure.
        if unsafe { GetFileInformationByHandle(opened.as_raw_handle(), &raw mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(u64::from(info.nNumberOfLinks))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn a_file_counts_each_of_its_names() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("one.md");
        fs::write(&file, "").unwrap();

        assert_eq!(links(&file).unwrap(), 1);
        fs::hard_link(&file, dir.join("two.md")).unwrap();
        assert_eq!(links(&file).unwrap(), 2);
        links(&dir.join("missing.md")).unwrap_err();
    }

    #[test]
    fn only_a_regular_file_with_one_name_opens_plain() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("one.md");
        fs::write(&file, "x").unwrap();

        open_plain(&file).unwrap();
        fs::hard_link(&file, dir.join("two.md")).unwrap();
        let refused = open_plain(&file).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
        open_plain(&dir).unwrap_err();
        open_plain(&dir.join("missing.md")).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_never_opens_plain() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("secret.json");
        fs::write(&file, "x").unwrap();
        let link = dir.join("link.jsonl");
        std::os::unix::fs::symlink(&file, &link).unwrap();

        open_plain(&link).unwrap_err();
        let refused = open_unfollowed(&link).unwrap_err();
        assert_ne!(refused.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_is_refused_without_waiting_for_a_writer() {
        let dir = TempDir::new().unwrap();
        let pipe = dir.join("pipe.jsonl");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        assert!(made.unwrap().success());

        let refused = open_plain(&pipe).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    }
}
