use std::ffi::OsString;
use std::fs::{File, OpenOptions};
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
    create_private(dir)
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
        #[expect(
            clippy::needless_pass_by_ref_mut,
            reason = "Windows' listener swaps in a new pipe on each accept"
        )]
        pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }

    pub(crate) fn create_private(dir: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
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
#[expect(
    unsafe_code,
    reason = "Windows has no safe API for access lists or pipe security"
)]
mod windows {
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::mem;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::signal::windows::{CtrlC, CtrlShutdown, ctrl_c, ctrl_shutdown};
    use weacon_client::platform::pipe_name;
    use weacon_client::platform::windows::current_user;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub(crate) type Stream = NamedPipeServer;

    /// Nothing to compare: the pipe's access list already admits only this user.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct Owner;

    pub(crate) struct Listener {
        name: String,
        user: String,
        next: NamedPipeServer,
    }

    impl Listener {
        /// The next client; a new pipe instance waits for the one after.
        pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
            self.next.connect().await?;
            let after = pipe(&self.name, &self.user, false)?;
            Ok(mem::replace(&mut self.next, after))
        }
    }

    /// Creates `dir`, making it private if it's new; parents get the access they inherit.
    pub(crate) fn create_private(dir: &Path) -> io::Result<()> {
        let new = !dir.exists();
        fs::DirBuilder::new().recursive(true).create(dir)?;
        if new {
            private_owner(dir)?;
        }
        Ok(())
    }

    /// Makes `dir`, the core's own folder, owned by this user and reachable only by them, the
    /// system and administrators, whatever it inherited.
    pub(crate) fn private_owner(dir: &Path) -> io::Result<Option<Owner>> {
        let user = current_user()?;
        let access = Descriptor::new(&format!(
            "O:{user}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{user})"
        ))?;
        let (mut owner, mut dacl) = (ptr::null_mut(), ptr::null_mut());
        let (mut present, mut defaulted) = (0, 0);
        // SAFETY: `access` outlives both calls, and the out-pointers outlive them.
        check(unsafe { GetSecurityDescriptorOwner(access.0, &raw mut owner, &raw mut defaulted) })?;
        // SAFETY: as above.
        check(unsafe {
            GetSecurityDescriptorDacl(
                access.0,
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted,
            )
        })?;
        let path = wide(dir.as_os_str())?;
        // SAFETY: `path` ends with a NUL, and `owner` and `dacl` point into `access`, which
        // outlives the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(status).unwrap_or(i32::MAX),
            ));
        }
        Ok(Some(Owner))
    }

    /// Listens on a pipe with a fresh random name that only this user can open, and writes the
    /// name to `path` for clients to find. An elevated daemon is refused, so that no ordinary
    /// program can drive it.
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        if elevated()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the daemon runs with administrator rights",
            ));
        }
        let user = current_user()?;
        let name = pipe_name(crate::report::random_id());
        let next = pipe(&name, &user, true)?;
        // Renamed into place, so a client never reads half a name.
        let unfinished = path.with_extension("new");
        fs::write(&unfinished, &name)?;
        fs::rename(&unfinished, path)?;
        Ok(Listener { name, user, next })
    }

    pub(crate) const fn is_owner(_: &Stream, _: Owner) -> bool {
        true
    }

    // A pipe instance owned by `user` and open only to them; others can't take its name, being
    // refused as later instances.
    fn pipe(name: &str, user: &str, first: bool) -> io::Result<NamedPipeServer> {
        let access = Descriptor::new(&format!("O:{user}D:P(A;;GA;;;{user})"))?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(io::Error::other)?,
            lpSecurityDescriptor: access.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` and the descriptor it points to outlive the call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
        }
    }

    fn elevated() -> io::Result<bool> {
        let mut token = ptr::null_mut();
        // SAFETY: the current process's pseudo-handle needs no closing, and `token` outlives the call.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) })?;
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let size = u32::try_from(size_of::<TOKEN_ELEVATION>()).map_err(io::Error::other);
        let mut written = 0;
        let read = size.and_then(|size| {
            // SAFETY: `elevation` holds `size` writable bytes, and both outlive the call.
            check(unsafe {
                GetTokenInformation(
                    token,
                    TokenElevation,
                    (&raw mut elevation).cast(),
                    size,
                    &raw mut written,
                )
            })
        });
        // SAFETY: `token` is open and isn't used after this.
        unsafe { CloseHandle(token) };
        read.map(|()| elevation.TokenIsElevated != 0)
    }

    /// A security descriptor built from SDDL, freed when dropped.
    struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Descriptor {
        fn new(sddl: &str) -> io::Result<Self> {
            let sddl = wide(OsStr::new(sddl))?;
            let mut descriptor = ptr::null_mut();
            // SAFETY: `sddl` ends with a NUL, and `descriptor` outlives the call.
            check(unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &raw mut descriptor,
                    ptr::null_mut(),
                )
            })?;
            Ok(Self(descriptor))
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: Windows allocated the descriptor with LocalAlloc, and it's freed only here.
            unsafe { LocalFree(self.0) };
        }
    }

    // A NUL-terminated wide string; one with a NUL inside would name something else.
    fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
        let units: Vec<u16> = text.encode_wide().collect();
        if units.contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        Ok(units.into_iter().chain([0]).collect())
    }

    fn check(succeeded: windows_sys::core::BOOL) -> io::Result<()> {
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
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
        use std::fs;
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
