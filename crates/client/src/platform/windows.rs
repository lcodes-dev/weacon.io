//! The daemon's pipe on Windows, open only to its user, used by the core too.

mod security;

use std::io;
use std::path::Path;
use std::time::Duration;

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

pub use security::make_private;
use security::{Descriptor, current_user, elevated, pipe_owner};

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
}
