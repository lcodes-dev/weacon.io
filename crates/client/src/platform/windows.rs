//! Windows' access controls for the daemon's pipe and state folder, used by the core too.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr;
use std::time::Duration;

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS,
    TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Connects to the daemon's pipe, whose name the daemon writes to `socket`, and checks that
/// this user owns it. tokio's client lets the server identify the user but not act as them.
pub(crate) async fn connect(socket: &Path) -> io::Result<Pipe> {
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
    // Only this user's own processes, and administrators, can make a pipe owned by them.
    if pipe_owner(&pipe)? != current_user()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "another user owns this pipe",
        ));
    }
    Ok(Pipe(pipe))
}

/// The client's end of the daemon's pipe. When the daemon hangs up by disconnecting the pipe,
/// reads end as at the end of a stream, as they do on a socket.
#[derive(Debug)]
pub(crate) struct Pipe(NamedPipeClient);

impl AsyncRead for Pipe {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let hung_up = i32::try_from(ERROR_PIPE_NOT_CONNECTED).ok();
        let polled = Pin::new(&mut self.0).poll_read(cx, buf);
        if let Poll::Ready(Err(error)) = &polled
            && error.raw_os_error() == hung_up
        {
            return Poll::Ready(Ok(()));
        }
        polled
    }
}

impl AsyncWrite for Pipe {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// A pipe instance named `name`, owned by this user and open only to them. An elevated
/// process's pipe is labelled high integrity, so only elevated programs can open it.
///
/// # Errors
/// When the pipe can't be made; with `first`, also when the name is taken.
pub fn create_pipe(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let user = current_user()?;
    let label = if elevated()? { "S:(ML;;NWNR;;;HI)" } else { "" };
    let access = Descriptor::new(&format!("O:{user}D:P(A;;GA;;;{user}){label}"))?;
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

/// Makes `dir` owned by this user and reachable only by them, the system and
/// administrators, whatever it inherited.
///
/// # Errors
/// When this user may not change who can reach `dir`.
pub fn make_private(dir: &Path) -> io::Result<()> {
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
    // The access list first: its entry for this user then grants the right to take ownership.
    let dacl_only = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    // SAFETY: `path` ends with a NUL, and `dacl` points into `access`, which outlives the call.
    let set = win32(unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            dacl_only,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    });
    set.map_err(|error| io::Error::new(error.kind(), format!("setting the access list: {error}")))?;
    // SAFETY: as above, with `owner`.
    let set = win32(unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            owner,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
        )
    });
    set.map_err(|error| io::Error::new(error.kind(), format!("setting the owner: {error}")))
}

/// The SID of the user running this process, such as `S-1-5-21-…`.
///
/// # Errors
/// When Windows won't say.
pub fn current_user() -> io::Result<String> {
    // Room for a TOKEN_USER and the longest SID, aligned for the struct.
    let mut buffer = [0_u64; 16];
    let len = size_of_val(&buffer);
    token_information(TokenUser, &mut buffer, len)?;
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into it.
    let sid = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read().User.Sid };
    sid_text(sid)
}

fn elevated() -> io::Result<bool> {
    let mut buffer = [0_u64; 1];
    token_information(TokenElevation, &mut buffer, size_of::<TOKEN_ELEVATION>())?;
    // SAFETY: on success the buffer starts with a TOKEN_ELEVATION.
    let elevation = unsafe { buffer.as_ptr().cast::<TOKEN_ELEVATION>().read() };
    Ok(elevation.TokenIsElevated != 0)
}

// Reads `class` of this process's token into `buffer`, as exactly `len` bytes when the class
// demands that.
fn token_information(
    class: TOKEN_INFORMATION_CLASS,
    buffer: &mut [u64],
    len: usize,
) -> io::Result<()> {
    let size = u32::try_from(len.min(size_of_val(buffer))).map_err(io::Error::other)?;
    let mut token = ptr::null_mut();
    // SAFETY: the current process's pseudo-handle needs no closing, and `token` outlives
    // the call.
    check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) })?;
    let mut written = 0;
    // SAFETY: `buffer` holds `size` writable bytes, and both outlive the call.
    let read = check(unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            size,
            &raw mut written,
        )
    });
    // SAFETY: `token` is open and isn't used after this.
    unsafe { CloseHandle(token) };
    read
}

fn pipe_owner(pipe: &impl AsRawHandle) -> io::Result<String> {
    let (mut owner, mut descriptor) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: the pipe stays open for the call; only the owner is asked for, and the
    // out-pointers outlive the call.
    win32(unsafe {
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
    })?;
    // `owner` points into the descriptor, so it's read before the descriptor is freed.
    let descriptor = Descriptor(descriptor);
    let owner = sid_text(owner);
    drop(descriptor);
    owner
}

fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: `sid` is valid for the call, and `text` outlives it.
    check(unsafe { ConvertSidToStringSidW(sid, &raw mut text) })?;
    Ok(take_text(text))
}

// Copies a NUL-terminated string that Windows allocated with LocalAlloc, then frees it.
fn take_text(text: *mut u16) -> String {
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
    string
}

/// A security descriptor that Windows allocated with `LocalAlloc`, freed when dropped.
pub(crate) struct Descriptor(pub(crate) PSECURITY_DESCRIPTOR);

impl Descriptor {
    pub(crate) fn new(sddl: &str) -> io::Result<Self> {
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

fn win32(status: u32) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ))
    }
}

/// A stand-in for the daemon's listener, for the tests.
#[cfg(test)]
pub(crate) struct Server(NamedPipeServer);

#[cfg(test)]
impl Server {
    pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
        use std::hash::{BuildHasher, RandomState};

        let id = RandomState::new().hash_one(socket);
        let name = super::pipe_name(u128::from(id));
        let server = create_pipe(&name, true)?;
        std::fs::write(socket, name)?;
        Ok(Self(server))
    }

    pub(crate) async fn accept(self) -> io::Result<NamedPipeServer> {
        self.0.connect().await?;
        Ok(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::pipe_name;
    use super::*;
    use weacon_testing::TempDir;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::OBJECT_SECURITY_INFORMATION;

    /// `dir`'s owner and access list, in SDDL.
    ///
    /// # Errors
    /// When `dir` can't be read.
    fn folder_access(dir: &Path) -> io::Result<String> {
        let path = wide(dir.as_os_str())?;
        let wanted = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let mut descriptor = ptr::null_mut();
        // SAFETY: `path` ends with a NUL; only the descriptor is asked for, and it outlives the call.
        win32(unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                wanted,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut descriptor,
            )
        })?;
        Descriptor(descriptor).text(wanted)
    }

    impl Descriptor {
        // The parts of this descriptor named by `wanted`, in SDDL.
        fn text(&self, wanted: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
            let mut text = ptr::null_mut();
            // SAFETY: the descriptor lives through the call, and `text` outlives it.
            check(unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    self.0,
                    SDDL_REVISION_1,
                    wanted,
                    &raw mut text,
                    ptr::null_mut(),
                )
            })?;
            Ok(take_text(text))
        }
    }

    fn name() -> String {
        use std::hash::{BuildHasher, RandomState};
        pipe_name(u128::from(RandomState::new().hash_one(1)))
    }

    #[tokio::test]
    async fn a_pipe_name_has_one_owner_and_strangers_pipes_are_refused() {
        let name = name();
        let ours = create_pipe(&name, true).unwrap();

        create_pipe(&name, true).unwrap_err();
        drop(ours);
        // Only an administrator can make a pipe another owner holds, as CI's runner can.
        let sddl = "O:BAD:P(A;;GA;;;WD)";
        let Ok(stranger) = Descriptor::new(sddl) else {
            return;
        };
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
            lpSecurityDescriptor: stranger.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` and the descriptor it points to outlive the call.
        let Ok(_theirs) = (unsafe {
            ServerOptions::new()
                .create_with_security_attributes_raw(&name, (&raw mut attributes).cast())
        }) else {
            return;
        };
        let dir = TempDir::new().unwrap();
        let socket = dir.join("stranger");
        std::fs::write(&socket, &name).unwrap();

        let refused = connect(&socket).await.unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_folder_is_made_ours_alone() {
        let dir = TempDir::new().unwrap();

        make_private(&dir).unwrap();
        make_private(&dir).expect("a private folder can be made private again");
        let access = folder_access(&dir).unwrap();
        // Windows writes some users' SIDs as aliases, such as LA for the built-in Administrator.
        let user = Descriptor::new(&format!("O:{}", current_user().unwrap()))
            .and_then(|owner| owner.text(OWNER_SECURITY_INFORMATION))
            .unwrap();

        assert!(access.starts_with(&format!("{user}D:P")), "{access}");
        assert!(
            !access.contains(";AU)") && !access.contains(";BU)"),
            "{access}"
        );
    }
}
