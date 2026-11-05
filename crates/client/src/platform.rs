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

        pub(crate) async fn accept(self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }
}

/// The Windows calls that check who runs the daemon, which the core uses too.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Windows has no safe API for token users or pipe owners"
)]
pub mod windows {
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::ptr;
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Connects to the daemon's pipe, whose name the daemon writes to `socket`, and checks that
    /// this user owns it. tokio's client lets the server identify the user but not act as them.
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
                Err(error) if error.raw_os_error() == i32::try_from(ERROR_PIPE_BUSY).ok() => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                opened => break opened?,
            }
        };
        // Only this user's own processes can make a pipe owned by them.
        if pipe_owner(&pipe)? != current_user()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "another user owns this pipe",
            ));
        }
        Ok(pipe)
    }

    /// The SID of the user running this process, such as `S-1-5-21-…`.
    ///
    /// # Errors
    /// When Windows won't say.
    pub fn current_user() -> io::Result<String> {
        let mut token = ptr::null_mut();
        // SAFETY: the current process's pseudo-handle needs no closing, and `token` outlives the call.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) })?;
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
        sid_text(sid)
    }

    fn pipe_owner(pipe: &impl AsRawHandle) -> io::Result<String> {
        let (mut owner, mut descriptor) = (ptr::null_mut(), ptr::null_mut());
        // SAFETY: the pipe stays open for the call; only the owner is asked for, and the
        // out-pointers outlive the call.
        let status = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &raw mut owner,
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
        let owner = sid_text(owner);
        // SAFETY: Windows allocated `descriptor`, which `owner` pointed into, with LocalAlloc.
        unsafe { LocalFree(descriptor) };
        owner
    }

    fn sid_text(sid: PSID) -> io::Result<String> {
        let mut text = ptr::null_mut();
        // SAFETY: `sid` is valid for the call, and `text` outlives it.
        check(unsafe { ConvertSidToStringSidW(sid, &raw mut text) })?;
        let mut length = 0;
        // SAFETY: `text` ends with a NUL and stays valid until it is freed below.
        while unsafe { text.add(length).read() } != 0 {
            length += 1;
        }
        // SAFETY: the `length` units before the NUL are initialised.
        let units = unsafe { std::slice::from_raw_parts(text, length) };
        let string = OsString::from_wide(units).to_string_lossy().into_owned();
        // SAFETY: Windows allocated `text` with LocalAlloc, and it isn't used after this.
        unsafe { LocalFree(text.cast()) };
        Ok(string)
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
    pub(crate) struct Server(tokio::net::windows::named_pipe::NamedPipeServer);

    #[cfg(test)]
    impl Server {
        pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
            use std::hash::{BuildHasher, RandomState};

            let id = RandomState::new().hash_one(socket);
            let name = super::pipe_name(u128::from(id));
            let server = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)?;
            std::fs::write(socket, name)?;
            Ok(Self(server))
        }

        pub(crate) async fn accept(
            self,
        ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
            self.0.connect().await?;
            Ok(self.0)
        }
    }
}

/// The daemon's pipe name for `id`.
#[must_use]
pub fn pipe_name(id: u128) -> String {
    format!(r"\\.\pipe\weacon-{id:032x}")
}

/// Whether `name` is a pipe name the daemon could have made, so no network path is opened.
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
    fn only_local_weacon_pipes_are_opened() {
        let good = pipe_name(0x0a0a_0a0a_0a0a_0a0a_0a0a_0a0a_0a0a_0a0a);

        assert_eq!(good, format!(r"\\.\pipe\weacon-{}", "0a".repeat(16)));
        assert!(is_pipe_name(&good));
        assert!(is_pipe_name(&pipe_name(1)));
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
