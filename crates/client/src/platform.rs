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
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

    const PIPE_BUSY: i32 = 231;

    /// Connects to the daemon's pipe, whose name the daemon writes to `socket`. tokio's client
    /// lets the server identify the user but not act as them.
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
        loop {
            match ClientOptions::new().open(&name) {
                Err(error) if error.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                connected => return connected,
            }
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
