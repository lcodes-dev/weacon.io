use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Reads an environment variable; tests pass a fake.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// An environment variable, where an empty one counts as unset.
pub(crate) fn var(env: Env<'_>, key: &str) -> Option<OsString> {
    env(key).filter(|value| !value.is_empty())
}

pub(crate) fn home_dir(env: Env<'_>) -> Option<PathBuf> {
    var(env, if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Where the core keeps its state; `WEACON_STATE_DIR` overrides it.
pub(crate) fn state_dir(env: Env<'_>) -> Option<PathBuf> {
    if let Some(dir) = var(env, "WEACON_STATE_DIR") {
        // A relative path would differ between commands started in different folders.
        return std::path::absolute(dir).ok();
    }
    if cfg!(windows) {
        return var(env, "LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Weacon"));
    }
    let home = home_dir(env)?;
    if cfg!(target_os = "macos") {
        return Some(home.join("Library/Application Support/Weacon"));
    }
    // The XDG spec says to ignore a relative path.
    let base = var(env, "XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"));
    Some(base.join("weacon"))
}

/// Creates `dir` and any missing parents readable only by this user; existing folders keep
/// their permissions.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Opens `path` for appending, creating it readable only by this user.
pub(crate) fn private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    pub(crate) use tokio::net::UnixStream as Stream;

    /// The user a client must be.
    pub(crate) type Owner = u32;

    pub(crate) struct Listener(tokio::net::UnixListener);

    impl Listener {
        pub(crate) async fn accept(&self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }

    /// The user that owns `dir`, when only that user can reach it.
    pub(crate) fn private_owner(dir: &Path) -> io::Result<Option<Owner>> {
        let metadata = fs::metadata(dir)?;
        let others = metadata.permissions().mode() & 0o077;
        let private = metadata.is_dir() && others == 0;
        Ok(private.then(|| metadata.uid()))
    }

    /// Listens on a socket at `path` that only this user can open.
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        let listener = tokio::net::UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Listener(listener))
    }

    pub(crate) fn is_owner(stream: &Stream, owner: Owner) -> bool {
        stream.peer_cred().is_ok_and(|cred| cred.uid() == owner)
    }

    /// Ctrl-C and SIGTERM, caught from when this is made; needs a tokio runtime.
    pub(crate) struct Signals {
        interrupt: Signal,
        terminate: Signal,
    }

    use tokio::signal::unix::{Signal, SignalKind, signal};

    impl Signals {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }

        pub(crate) async fn wait(mut self) {
            tokio::select! {
                _ = self.interrupt.recv() => {}
                _ = self.terminate.recv() => {}
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::io;
    use std::path::Path;

    use interprocess::os::windows::named_pipe::tokio::{DuplexPipeStream, PipeListener};
    use interprocess::os::windows::named_pipe::{PipeListenerOptions, pipe_mode::Bytes};
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use tokio::signal::windows::{CtrlC, CtrlShutdown, ctrl_c, ctrl_shutdown};
    use weacon_client::platform::only_user_and_admins;
    use weacon_client::platform::windows::{current_user, folder_access};
    use widestring::U16CString;

    pub(crate) type Stream = DuplexPipeStream<Bytes>;

    /// Nothing to compare: the pipe's access list already admits only its owner.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct Owner;

    pub(crate) struct Listener(PipeListener<Bytes, Bytes>);

    impl Listener {
        pub(crate) async fn accept(&self) -> io::Result<Stream> {
            self.0.accept().await
        }
    }

    /// Whether only this user, the system and administrators can reach `dir`.
    pub(crate) fn private_owner(dir: &Path) -> io::Result<Option<Owner>> {
        let private = only_user_and_admins(&folder_access(dir)?, &current_user()?);
        Ok(private.then_some(Owner))
    }

    /// Listens on a pipe with a fresh random name that only this user can open, and writes the
    /// name to `path` for clients to find; others can't take the name before the daemon does.
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        let name = format!(r"\\.\pipe\weacon-{:032x}", crate::report::random_id());
        // Named by SID, as an elevated user's objects are otherwise owned by Administrators.
        let user = current_user()?;
        let owner_only = U16CString::from_str(format!("O:{user}D:P(A;;GA;;;{user})"))
            .map_err(io::Error::other)?;
        let owner_only = SecurityDescriptor::deserialize(&owner_only)?;
        let listener = PipeListenerOptions::new()
            .path(name.as_str())
            .security_descriptor(Some(owner_only))
            .create_tokio_duplex()?;
        // Renamed into place, so a client never reads half a name.
        let unfinished = path.with_extension("new");
        fs::write(&unfinished, name)?;
        fs::rename(&unfinished, path)?;
        Ok(Listener(listener))
    }

    pub(crate) const fn is_owner(_: &Stream, _: Owner) -> bool {
        true
    }

    /// Ctrl-C and shutdown, caught from when this is made; needs a tokio runtime.
    pub(crate) struct Signals {
        interrupt: CtrlC,
        shutdown: CtrlShutdown,
    }

    impl Signals {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Self {
                interrupt: ctrl_c()?,
                shutdown: ctrl_shutdown()?,
            })
        }

        pub(crate) async fn wait(mut self) {
            tokio::select! {
                _ = self.interrupt.recv() => {}
                _ = self.shutdown.recv() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env;

    #[cfg(windows)]
    #[test]
    fn on_windows_the_state_folder_is_in_local_app_data() {
        let vars = env(&[
            ("LOCALAPPDATA", r"C:\Users\ada\AppData\Local"),
            ("USERPROFILE", r"C:\Users\ada"),
        ]);

        assert_eq!(
            state_dir(&vars),
            Some(PathBuf::from(r"C:\Users\ada\AppData\Local\Weacon"))
        );
        assert_eq!(home_dir(&vars), Some(PathBuf::from(r"C:\Users\ada")));
        assert_eq!(state_dir(&env(&[])), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_folder_can_be_set() {
        let set = env(&[("WEACON_STATE_DIR", "/srv/weacon"), ("HOME", "/home/ada")]);
        let relative = env(&[("WEACON_STATE_DIR", ".dev/state")]);

        assert_eq!(state_dir(&set), Some(PathBuf::from("/srv/weacon")));
        assert_eq!(home_dir(&set), Some(PathBuf::from("/home/ada")));
        assert_eq!(
            state_dir(&relative),
            Some(std::env::current_dir().unwrap().join(".dev/state"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_the_state_folder_follows_xdg() {
        let state = |vars: &[(&str, &str)]| state_dir(&env(vars));
        let home = ("HOME", "/home/ada");
        let default = Some(PathBuf::from("/home/ada/.local/state/weacon"));

        assert_eq!(state(&[home]), default);
        assert_eq!(
            state(&[home, ("XDG_STATE_HOME", "/data/state")]),
            Some(PathBuf::from("/data/state/weacon"))
        );
        assert_eq!(state(&[home, ("XDG_STATE_HOME", "state")]), default);
        assert_eq!(state(&[home, ("WEACON_STATE_DIR", "")]), default);
        assert_eq!(state(&[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn new_folders_and_files_are_private_and_existing_ones_kept() {
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_support::temp_dir("private");
        let nested = dir.join("state/logs");
        let file = nested.join("weacon.log");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&nested).unwrap();
        private_dir(&dir).unwrap();
        drop(private_append(&file).unwrap());
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode(&dir), 0o755);
        assert_eq!(mode(&dir.join("state")), 0o700);
        assert_eq!(mode(&nested), 0o700);
        assert_eq!(mode(&file), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }
}
