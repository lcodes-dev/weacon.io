use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(unix)]
use tokio::signal::unix::{Signal as Interrupt, Signal as Stop, SignalKind, signal};
#[cfg(windows)]
use tokio::signal::windows::{CtrlC as Interrupt, CtrlShutdown as Stop, ctrl_c, ctrl_shutdown};

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

/// Ctrl-C and the system's request to stop, caught from when this is made; needs a tokio
/// runtime.
pub struct Signals {
    interrupt: Interrupt,
    stop: Stop,
}

impl Signals {
    /// # Errors
    /// When the signals can't be caught.
    #[cfg(unix)]
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            stop: signal(SignalKind::terminate())?,
        })
    }

    /// # Errors
    /// When the signals can't be caught.
    #[cfg(windows)]
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            interrupt: ctrl_c()?,
            stop: ctrl_shutdown()?,
        })
    }

    pub async fn wait(mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.stop.recv() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn new_files_are_private() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_support::temp_dir("private");
        let file = dir.join("weacon.log");
        drop(super::private_append(&file).unwrap());

        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn signals_wait_for_the_request_to_stop() {
        use std::process::Command;
        use std::time::Duration;

        let signals = super::Signals::new().unwrap();
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
