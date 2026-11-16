#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(all(windows, test))]
pub(crate) use windows::Server;
#[cfg(windows)]
pub(crate) use windows::{Pipe as Stream, connect};

#[cfg(unix)]
mod unix {
    use std::io;
    use std::path::Path;

    pub(crate) use tokio::net::UnixStream as Stream;

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

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Windows has no safe API for access lists or token users"
)]
pub mod windows;

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
