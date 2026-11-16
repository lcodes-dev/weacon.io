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

/// Takes this process's lock on all of `file`, waiting while another process has it. A child
/// never inherits the handle, so it never keeps the lock.
pub(crate) fn lock_for_process(file: &fs::File) -> io::Result<()> {
    file.lock()
}

/// Removes `dir` and all it holds, read-only files too, which Windows won't delete as they are.
pub(crate) fn remove_tree(dir: &Path) -> io::Result<()> {
    writable(dir)?;
    fs::remove_dir_all(dir)
}

// A link is left as it is, so no file outside `dir` changes.
fn writable(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            writable(&entry.path())?;
        } else if kind.is_file() {
            let mut permissions = entry.metadata()?.permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows has no bit for others to write; only this flag stops the delete"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(entry.path(), permissions)?;
        }
    }
    Ok(())
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

/// Creates `dir` and makes it private; parents get the access they inherit.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).create(dir)?;
    make_private(dir)
}

/// Windows runs any file whose name ends in `.exe`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the same call as on Unix, where it can fail"
)]
pub(crate) fn make_runnable(_file: &Path) -> io::Result<()> {
    Ok(())
}

pub(crate) fn is_runnable(file: &Path) -> bool {
    file.is_file()
}

/// Listens on a pipe with a fresh random name that only this user can open, and writes the
/// name to `path` for clients to find.
pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
    let name = pipe_name(weacon_appkit::random_id());
    let next = create_pipe(&name, true)?;
    weacon_appkit::write_whole(path, name.as_bytes())?;
    Ok(Listener { name, next })
}
