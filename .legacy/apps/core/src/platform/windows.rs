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
        let after = create_pipe(&self.name)?;
        Ok(Stream(mem::replace(&mut self.next, after)))
    }
}

/// Takes this process's lock on all of `file`, waiting while another process has it. A child
/// never inherits the handle, so it never keeps the lock.
pub(crate) fn lock_for_process(file: &fs::File) -> io::Result<()> {
    file.lock()
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

/// Creates `dir` and makes it this user's alone, whatever it was.
pub(crate) fn own_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).create(dir)?;
    make_private(dir)
}

pub(crate) fn is_runnable(file: &Path) -> bool {
    file.is_file()
}

/// Listens on a pipe with a fresh random name that only this user can open, and writes the
/// name to `path` for clients to find.
pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
    let name = pipe_name(weacon_appkit::random_id());
    let next = create_pipe(&name)?;
    weacon_appkit::write_whole(path, name.as_bytes())?;
    Ok(Listener { name, next })
}
