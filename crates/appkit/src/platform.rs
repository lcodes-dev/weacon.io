use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Whether only its owner can reach the file or folder `metadata` is of; `None` where the
/// system keeps access lists instead of permissions, as Windows does, and this can't tell.
#[must_use]
pub fn owner_only(metadata: &Metadata) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let others = metadata.permissions().mode() & 0o077;
        Some(others == 0)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// Makes what was created, renamed or removed in `dir` survive a power loss; an empty `dir`
/// is the current folder.
///
/// # Errors
/// When `dir` isn't there or can't be synced.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    // Windows can't open a folder, and keeps a rename once it has returned.
    #[cfg(not(unix))]
    {
        fs::metadata(dir).map(drop)
    }
}

/// Writes `contents` to `path` whole, through a file synced and renamed into place, so no
/// reader sees half of it, a crash keeps the old or the new, and what was written stays
/// written; creates missing parent folders.
///
/// # Errors
/// When a folder or the file can't be created, written or renamed.
pub fn write_whole(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let unfinished = path.with_extension("new");
    let mut file = File::create(&unfinished)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&unfinished, path)?;
    path.parent().map_or(Ok(()), sync_dir)
}

/// Opens `path` for appending, creating it readable only by this user.
///
/// # Errors
/// When the file can't be opened or created.
pub fn private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    // Readable too: Windows locks only a file opened for reading or writing.
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(unix)]
pub use unix::Signals;
#[cfg(windows)]
pub use windows::Signals;

#[cfg(unix)]
mod unix {
    use std::io;

    use tokio::signal::unix::{Signal, SignalKind, signal};

    /// Ctrl-C and SIGTERM, caught from when this is made; needs a tokio runtime.
    pub struct Signals {
        interrupt: Signal,
        terminate: Signal,
    }

    impl Signals {
        /// # Errors
        /// When the signals can't be caught.
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }

        pub async fn wait(mut self) {
            tokio::select! {
                _ = self.interrupt.recv() => {}
                _ = self.terminate.recv() => {}
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use tokio::signal::windows::{
        CtrlC, CtrlClose, CtrlShutdown, ctrl_c, ctrl_close, ctrl_shutdown,
    };

    /// Ctrl-C, the console closing and shutdown, caught from when this is made; needs a tokio
    /// runtime.
    pub struct Signals {
        interrupt: CtrlC,
        close: CtrlClose,
        shutdown: CtrlShutdown,
    }

    impl Signals {
        /// # Errors
        /// When the signals can't be caught.
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                interrupt: ctrl_c()?,
                close: ctrl_close()?,
                shutdown: ctrl_shutdown()?,
            })
        }

        pub async fn wait(mut self) {
            tokio::select! {
                _ = self.interrupt.recv() => {}
                _ = self.close.recv() => {}
                _ = self.shutdown.recv() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn a_folder_that_isnt_there_cant_be_synced() {
        let dir = TempDir::new().unwrap();

        sync_dir(&dir).unwrap();
        sync_dir(Path::new("")).unwrap();
        sync_dir(&dir.join("missing")).unwrap_err();
    }

    #[test]
    fn a_whole_write_creates_folders_and_replaces_the_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("a/b/unit.service");

        write_whole(&path, b"first").unwrap();
        write_whole(&path, b"second").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        assert!(!path.with_extension("new").exists());
        assert!(write_whole(&dir.join("a/b/unit.service/x"), b"").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_or_write_is_not_private() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let file = dir.join("file");
        fs::write(&file, "").unwrap();
        let private_with = |mode| {
            fs::set_permissions(&file, Permissions::from_mode(mode)).unwrap();
            owner_only(&fs::metadata(&file).unwrap()).unwrap()
        };

        assert!(private_with(0o600));
        assert!(private_with(0o400));
        assert!(private_with(0o700));
        assert!(!private_with(0o640));
        assert!(!private_with(0o604));
        assert!(!private_with(0o602));
        assert!(!private_with(0o610));
    }

    #[cfg(not(unix))]
    #[test]
    fn without_permissions_no_file_can_be_told_private() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("file");
        fs::write(&file, "").unwrap();

        assert_eq!(owner_only(&fs::metadata(&file).unwrap()), None);
    }

    #[cfg(unix)]
    #[test]
    fn new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let file = dir.join("weacon.log");
        drop(private_append(&file).unwrap());

        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn signals_wait_for_the_request_to_stop() {
        use std::process::Command;
        use std::time::Duration;

        let signals = Signals::new().unwrap();
        let mut wait = Box::pin(signals.wait());
        tokio::time::timeout(Duration::from_secs(1), &mut wait)
            .await
            .unwrap_err();
        let pid = std::process::id().to_string();
        let sent = Command::new("kill").args(["-TERM", &pid]).status().unwrap();

        assert!(sent.success());
        wait.await;
    }
}
