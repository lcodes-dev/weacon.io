//! Where the fake core listens.

use std::io;
use std::path::Path;

/// A socket only this user can reach, at `path`.
#[cfg(unix)]
pub type Listener = tokio::net::UnixListener;

/// Listens at `path`, replacing anything left there.
///
/// # Errors
/// When the socket can't be made.
#[cfg(unix)]
pub fn listen(path: &Path) -> io::Result<Listener> {
    use std::os::unix::fs::PermissionsExt;

    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    let listener = Listener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The fake core listens only on Unix sockets for now.
#[cfg(windows)]
pub type Listener = std::convert::Infallible;

/// The fake core listens only on Unix sockets for now.
///
/// # Errors
/// Always.
#[cfg(windows)]
pub fn listen(_: &Path) -> io::Result<Listener> {
    Err(io::ErrorKind::Unsupported.into())
}
