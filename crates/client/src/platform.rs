#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(all(windows, test))]
pub(crate) use windows::Server;
#[cfg(windows)]
pub(crate) use windows::connect;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::path::Path;

    use tokio::net::UnixStream as Stream;

    /// Connects to the daemon's socket at `socket`.
    pub(crate) async fn connect(socket: &Path) -> io::Result<Stream> {
        Stream::connect(socket).await
    }

    /// A stand-in for the daemon's listener, for the tests.
    #[cfg(test)]
    pub(crate) struct Server(tokio::net::UnixListener);

    #[cfg(test)]
    impl Server {
        pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
            tokio::net::UnixListener::bind(socket).map(Self)
        }

        pub(crate) async fn accept(&self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }
}

/// Windows' own checks of who runs what, which the core uses too.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Windows has no safe API for token users and access lists"
)]
pub mod windows {
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::ptr;
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const PIPE_BUSY: i32 = 231;

    /// Connects to the daemon's pipe, whose name the daemon writes to `socket`, and checks that
    /// this user runs it. tokio's client lets the server identify the user but not act as them.
    pub(crate) async fn connect(socket: &Path) -> io::Result<NamedPipeClient> {
        let mut name = String::new();
        tokio::fs::File::open(socket)
            .await?
            .take(64)
            .read_to_string(&mut name)
            .await?;
        if !super::is_pipe_name(&name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a Weacon pipe name",
            ));
        }
        let pipe = loop {
            match ClientOptions::new().open(&name) {
                Err(error) if error.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                opened => break opened?,
            }
        };
        if pipe_server_user(&pipe)? != current_user()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "another user runs this pipe",
            ));
        }
        Ok(pipe)
    }

    /// The SID of the user running this process, such as `S-1-5-21-…`.
    ///
    /// # Errors
    /// When Windows won't say.
    pub fn current_user() -> io::Result<String> {
        // SAFETY: the pseudo-handle of the current process needs no closing.
        process_user(unsafe { GetCurrentProcess() })
    }

    /// `path`'s access list, in SDDL, such as `D:PAI(A;OICI;FA;;;SY)`.
    ///
    /// # Errors
    /// When the path can't be read.
    pub fn folder_access(path: &Path) -> io::Result<String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut descriptor = ptr::null_mut();
        // SAFETY: `wide` ends with a NUL; only the descriptor is asked for, and it outlives the call.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(status).unwrap_or(i32::MAX),
            ));
        }
        let mut text = ptr::null_mut();
        // SAFETY: `descriptor` came from the call above, and `text` outlives this one.
        let converted = check(unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &raw mut text,
                ptr::null_mut(),
            )
        });
        // SAFETY: Windows allocated `descriptor` with LocalAlloc; it isn't used after this.
        unsafe { LocalFree(descriptor) };
        converted?;
        Ok(take_local_string(text))
    }

    fn pipe_server_user(pipe: &impl AsRawHandle) -> io::Result<String> {
        let mut pid = 0;
        // SAFETY: the pipe stays open for the call, and `pid` outlives it.
        check(unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &raw mut pid) })?;
        // SAFETY: a plain call; a null handle means it failed.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let user = process_user(process);
        // SAFETY: `process` is open and isn't used after this.
        unsafe { CloseHandle(process) };
        user
    }

    fn process_user(process: HANDLE) -> io::Result<String> {
        let mut token = ptr::null_mut();
        // SAFETY: `token` outlives the call.
        check(unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) })?;
        let user = token_user(token);
        // SAFETY: `token` is open and isn't used after this.
        unsafe { CloseHandle(token) };
        user
    }

    fn token_user(token: HANDLE) -> io::Result<String> {
        // Room for a TOKEN_USER and the longest SID, aligned for the struct.
        let mut buffer = [0_u64; 16];
        let size = u32::try_from(size_of_val(&buffer)).map_err(io::Error::other)?;
        let mut written = 0;
        // SAFETY: `buffer` holds `size` writable bytes, and both outlive the call.
        check(unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &raw mut written,
            )
        })?;
        // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into it.
        let sid = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read().User.Sid };
        let mut text = ptr::null_mut();
        // SAFETY: `sid` is valid while `buffer` lives, and `text` outlives the call.
        check(unsafe { ConvertSidToStringSidW(sid, &raw mut text) })?;
        Ok(take_local_string(text))
    }

    // Copies a NUL-terminated string that Windows allocated with LocalAlloc, then frees it.
    fn take_local_string(text: *mut u16) -> String {
        let mut length = 0;
        // SAFETY: `text` ends with a NUL and stays valid until it is freed below.
        while unsafe { text.add(length).read() } != 0 {
            length += 1;
        }
        // SAFETY: the `length` units before the NUL are initialised.
        let units = unsafe { std::slice::from_raw_parts(text, length) };
        let string = OsString::from_wide(units).to_string_lossy().into_owned();
        // SAFETY: `text` came from LocalAlloc and isn't used after this.
        unsafe { LocalFree(text.cast()) };
        string
    }

    fn check(succeeded: windows_sys::core::BOOL) -> io::Result<()> {
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// A stand-in for the daemon's listener, for the tests.
    #[cfg(test)]
    pub(crate) struct Server(
        std::sync::Mutex<Option<tokio::net::windows::named_pipe::NamedPipeServer>>,
    );

    #[cfg(test)]
    impl Server {
        pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
            use std::hash::{BuildHasher, RandomState};

            let half = || RandomState::new().hash_one(socket);
            let name = format!(r"\\.\pipe\weacon-{:016x}{:016x}", half(), half());
            let server = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)?;
            std::fs::write(socket, name)?;
            Ok(Self(std::sync::Mutex::new(Some(server))))
        }

        pub(crate) async fn accept(
            &self,
        ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
            let server = self.0.lock().ok().and_then(|mut slot| slot.take());
            let server = server.ok_or_else(|| io::Error::other("accepts one client"))?;
            server.connect().await?;
            Ok(server)
        }
    }
}

/// Whether an access list, in SDDL, lets in only `user`, the system and administrators.
#[must_use]
pub fn only_user_and_admins(sddl: &str, user: &str) -> bool {
    let Some(dacl) = sddl.strip_prefix("D:") else {
        return false;
    };
    // An access list with no entries for `user` still admits the owner, which may be anyone.
    !dacl.starts_with("NO_ACCESS_CONTROL")
        && dacl.split('(').skip(1).all(|ace| {
            let fields: Vec<&str> = ace.trim_end_matches(')').split(';').collect();
            let allows = matches!(fields.first(), Some(&("A" | "OA")));
            !allows
                || fields
                    .get(5)
                    .is_some_and(|who| [user, "SY", "BA", "OW", "CO"].contains(who))
        })
}

/// Whether `name` is a local pipe the daemon could have made, so no network path is opened.
#[cfg_attr(
    all(unix, not(test)),
    expect(dead_code, reason = "only Windows connects by pipe name")
)]
fn is_pipe_name(name: &str) -> bool {
    name.strip_prefix(r"\\.\pipe\weacon-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_user_the_system_and_admins_may_be_let_in() {
        let user = "S-1-5-21-1-2-3-1001";
        let profile = format!("D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{user})");
        let shared = "D:AI(A;OICI;FA;;;SY)(A;OICIID;0x1301bf;;;AU)";

        assert!(only_user_and_admins(&profile, user));
        assert!(only_user_and_admins(
            &format!("{profile}(D;;FA;;;WD)"),
            user
        ));
        assert!(!only_user_and_admins(shared, user));
        assert!(!only_user_and_admins(&profile, "S-1-5-21-1-2-3-1002"));
        assert!(!only_user_and_admins("D:NO_ACCESS_CONTROL", user));
        assert!(!only_user_and_admins("O:BA", user));
    }

    #[test]
    fn only_local_weacon_pipes_are_opened() {
        let good = format!(r"\\.\pipe\weacon-{}", "0a".repeat(16));

        assert!(is_pipe_name(&good));
        for bad in [
            format!(r"\\host\pipe\weacon-{}", "0a".repeat(16)),
            format!(r"\\.\pipe\weacon-{}", "0A".repeat(16)),
            format!(r"\\.\pipe\weacon-{}", "0a".repeat(15)),
            format!(r"\\.\pipe\other-{}", "0a".repeat(16)),
            format!("{good}\n"),
        ] {
            assert!(!is_pipe_name(&bad), "{bad}");
        }
    }
}
