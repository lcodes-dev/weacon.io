use std::io;
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
pub(crate) type Stream = std::os::unix::net::UnixStream;

/// Connects to the daemon's socket, with `timeout` on every read and write.
#[cfg(unix)]
pub(crate) fn connect(path: &Path, timeout: Duration) -> io::Result<Stream> {
    let stream = Stream::connect(path)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}
