use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) use tokio::net::UnixStream as Stream;

#[cfg(windows)]
pub(crate) type Stream = interprocess::os::windows::named_pipe::tokio::DuplexPipeStream<
    interprocess::os::windows::named_pipe::pipe_mode::Bytes,
>;

/// Connects to the daemon's socket at `socket`.
#[cfg(unix)]
pub(crate) async fn connect(socket: &Path) -> io::Result<Stream> {
    Stream::connect(socket).await
}

/// Connects to the daemon's pipe, whose name the daemon writes to `socket`.
#[cfg(windows)]
pub(crate) async fn connect(socket: &Path) -> io::Result<Stream> {
    let name = tokio::fs::read_to_string(socket).await?;
    Stream::connect_by_path(name.trim()).await
}

#[cfg(test)]
pub(crate) use server::Server;

/// A stand-in for the daemon's listener, for the tests.
#[cfg(test)]
mod server {
    use std::io;
    use std::path::Path;

    use super::Stream;

    #[cfg(unix)]
    pub(crate) struct Server(tokio::net::UnixListener);

    #[cfg(unix)]
    impl Server {
        pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
            tokio::net::UnixListener::bind(socket).map(Self)
        }

        pub(crate) async fn accept(&self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }

    #[cfg(windows)]
    pub(crate) struct Server(
        interprocess::os::windows::named_pipe::tokio::PipeListener<
            interprocess::os::windows::named_pipe::pipe_mode::Bytes,
            interprocess::os::windows::named_pipe::pipe_mode::Bytes,
        >,
    );

    #[cfg(windows)]
    impl Server {
        pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
            use std::hash::BuildHasher;

            use interprocess::os::windows::named_pipe::PipeListenerOptions;

            let id = std::hash::RandomState::new().hash_one(socket);
            let name = format!(r"\\.\pipe\weacon-client-test-{id:016x}");
            let listener = PipeListenerOptions::new()
                .path(name.as_str())
                .create_tokio_duplex()?;
            std::fs::write(socket, name)?;
            Ok(Self(listener))
        }

        pub(crate) async fn accept(&self) -> io::Result<Stream> {
            self.0.accept().await
        }
    }
}
