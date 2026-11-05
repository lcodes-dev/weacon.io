#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;

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

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;

    use interprocess::os::windows::named_pipe::pipe_mode::Bytes;
    use interprocess::os::windows::named_pipe::tokio::DuplexPipeStream;

    type Stream = DuplexPipeStream<Bytes>;

    /// Connects to the daemon's pipe, whose name the daemon writes to `socket`.
    pub(crate) async fn connect(socket: &Path) -> io::Result<Stream> {
        let name = tokio::fs::read_to_string(socket).await?;
        Stream::connect_by_path(name.as_str()).await
    }

    /// A stand-in for the daemon's listener, for the tests.
    #[cfg(test)]
    pub(crate) struct Server(
        interprocess::os::windows::named_pipe::tokio::PipeListener<Bytes, Bytes>,
    );

    #[cfg(test)]
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
