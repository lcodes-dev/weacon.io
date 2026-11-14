//! Opening a file beneath a folder without following a link at any step.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
use unix::open_parts;
#[cfg(windows)]
use windows::open_parts;

/// The file at `relative`, a `/`-separated path, beneath the folder `root`, opened to read. A
/// link anywhere on the way, a part that isn't one plain name, and a path that isn't there are
/// refused.
pub(crate) fn open_beneath(root: &Path, relative: &str) -> io::Result<File> {
    let parts: Vec<&str> = relative.split('/').collect();
    if !parts.iter().all(|part| is_name(part)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a plain path",
        ));
    }
    open_parts(root, &parts)
}

/// One file or folder name, the same on every system: `\` and `:` separate or root a path on
/// Windows, so they're refused everywhere.
fn is_name(part: &str) -> bool {
    !matches!(part, "" | "." | "..") && !part.contains(['\\', ':', '\0'])
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::Path;

    use libc::{O_CLOEXEC, O_DIRECTORY, O_NOFOLLOW, O_NONBLOCK, O_RDONLY, c_int};

    const FOLDER: c_int = O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_DIRECTORY;
    /// Non-blocking, so a pipe can't hold the open.
    const FILE: c_int = O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK;

    /// Each part is opened from the folder before it with `O_NOFOLLOW`, so a link swapped in
    /// at any time is refused rather than followed.
    #[expect(unsafe_code, reason = "std has no openat")]
    pub(super) fn open_parts(root: &Path, parts: &[&str]) -> io::Result<File> {
        let mut opened = File::open(root)?;
        let last = parts.len().saturating_sub(1);
        for (at, part) in parts.iter().enumerate() {
            let name = CString::new(*part)?;
            let flags = if at == last { FILE } else { FOLDER };
            // SAFETY: the folder's descriptor stays open for the call, and `name` ends in NUL.
            let fd = unsafe { libc::openat(opened.as_raw_fd(), name.as_ptr(), flags) };
            if fd == -1 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` was just opened here and nothing else owns it.
            opened = unsafe { File::from_raw_fd(fd) };
        }
        Ok(opened)
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    /// Every folder on the way must be no reparse point, and the file is opened as itself.
    pub(super) fn open_parts(root: &Path, parts: &[&str]) -> io::Result<File> {
        let mut path = root.to_path_buf();
        for part in parts {
            path.push(part);
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "a link"));
            }
        }
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
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
    fn opens_plain_paths_only() {
        let dir = TempDir::new().unwrap();
        let root = dir.join("root");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/c.md"), "inside").unwrap();
        fs::write(dir.join("outside.md"), "outside").unwrap();
        assert_eq!(read(&root, "a/b/c.md").as_deref(), Some("inside"));
        for refused in [
            "a//b/c.md",
            "./a/b/c.md",
            "a/../a/b/c.md",
            "../outside.md",
            "",
            "a/b/x.md",
            "..\\outside.md",
            "C:outside.md",
            "a/b\0/c.md",
        ] {
            assert_eq!(read(&root, refused), None, "{refused}");
        }
        // Plain names on Unix, but a path on Windows: refused everywhere alike.
        #[cfg(unix)]
        for name in ["a\\b\\c.md", "a:b.md"] {
            fs::write(root.join(name), "inside").unwrap();
            assert_eq!(read(&root, name), None, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn follows_no_link_at_any_step() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let root = dir.join("root");
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/c.md"), "inside").unwrap();
        symlink(root.join("real"), root.join("folder")).unwrap();
        symlink(root.join("real/c.md"), root.join("file.md")).unwrap();
        assert_eq!(read(&root, "real/c.md").as_deref(), Some("inside"));
        assert_eq!(read(&root, "folder/c.md"), None);
        assert_eq!(read(&root, "file.md"), None);

        let made = std::process::Command::new("mkfifo")
            .arg(root.join("pipe.md"))
            .status()
            .unwrap();
        assert!(made.success());
        let pipe = open_beneath(&root, "pipe.md").unwrap();
        assert!(!pipe.metadata().unwrap().is_file());
    }
}
