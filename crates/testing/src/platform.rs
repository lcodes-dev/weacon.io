//! How the fake core listens and test folders are made, per OS.

#[cfg(unix)]
pub(crate) use unix::create_own_dir;
#[cfg(unix)]
pub use unix::{Listener, listen};
#[cfg(windows)]
pub(crate) use windows::create_own_dir;
#[cfg(windows)]
pub use windows::{Listener, listen};

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// A socket only this user can reach.
    pub type Listener = tokio::net::UnixListener;

    /// Creates the folder `path`, readable only by this user.
    pub(crate) fn create_own_dir(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new().mode(0o700).create(path)
    }

    /// Listens at `path`, replacing anything left there.
    ///
    /// # Errors
    /// When the socket can't be made.
    pub fn listen(path: &Path) -> io::Result<Listener> {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        let listener = Listener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }
}

#[cfg(windows)]
mod windows {
    use std::future::{Ready, ready};
    use std::io;
    use std::path::Path;

    /// Creates the folder `path`, which inherits its parent's access list.
    pub(crate) fn create_own_dir(path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }

    /// The fake core listens only on Unix sockets for now, so nothing connects here.
    #[derive(Debug)]
    pub struct Listener;

    impl Listener {
        /// Never accepts anyone.
        ///
        /// # Errors
        /// Always.
        pub fn accept(&self) -> Ready<io::Result<(tokio::io::DuplexStream, ())>> {
            ready(Err(io::ErrorKind::Unsupported.into()))
        }
    }

    /// The fake core listens only on Unix sockets for now.
    ///
    /// # Errors
    /// Always.
    pub fn listen(_: &Path) -> io::Result<Listener> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn listens_privately_and_replaces_a_stale_socket() {
        let dir = crate::TempDir::new().unwrap();
        fs::create_dir(dir.join("taken")).unwrap();
        let path = dir.join("weacon.sock");
        drop(listen(&path).unwrap());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);
        listen(&path).unwrap();
        listen(&dir.join("taken")).unwrap_err();
    }
}
