use std::fs;
use std::io;
use std::mem;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::NamedPipeServer;
use weacon_client::platform::pipe_name;
use weacon_client::platform::windows::{create_pipe, make_private};

/// A client's pipe, disconnected when dropped: a pipe with a write still pending, as to a
/// client that never reads, otherwise stays open until that write finishes.
pub(crate) struct Stream(NamedPipeServer);

impl Drop for Stream {
    fn drop(&mut self) {
        // A pipe already gone has nothing left to close.
        self.0.disconnect().unwrap_or_default();
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
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

/// Nothing to compare: the pipe's access list already admits only this user.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Owner;

pub(crate) struct Listener {
    name: String,
    next: NamedPipeServer,
}

impl Listener {
    /// The next client; a new pipe instance waits for the one after.
    pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
        self.next.connect().await?;
        let after = create_pipe(&self.name, false)?;
        Ok(Stream(mem::replace(&mut self.next, after)))
    }
}

/// Bytes this user may still write on the disk holding `dir`.
#[expect(unsafe_code, reason = "GetDiskFreeSpaceExW has no safe API in std")]
pub(crate) fn free_space(dir: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut available = 0_u64;
    // SAFETY: `wide` is NUL-terminated, `available` is writable and the totals may be null.
    let done = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

/// Creates `dir`, making it private if it's new; parents get the access they inherit.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    let new = !dir.exists();
    fs::DirBuilder::new().recursive(true).create(dir)?;
    if new {
        make_private(dir)?;
    }
    Ok(())
}

pub(crate) fn is_runnable(file: &Path) -> bool {
    file.is_file()
}

/// Makes `dir`, the core's own folder, private to this user, whatever it inherited.
pub(crate) fn private_owner(dir: &Path) -> io::Result<Option<Owner>> {
    make_private(dir)?;
    Ok(Some(Owner))
}

/// Listens on a pipe with a fresh random name that only this user can open, and writes the
/// name to `path` for clients to find.
pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
    let name = pipe_name(weacon_appkit::random_id());
    let next = create_pipe(&name, true)?;
    weacon_appkit::write_whole(path, name.as_bytes())?;
    Ok(Listener { name, next })
}

pub(crate) const fn is_owner(_: &Stream, _: Owner) -> bool {
    true
}
