//! Where the fake core listens.

#[cfg(unix)]
pub use unix::{Listener, listen};
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
    use std::io;
    use std::path::Path;

    /// The fake core listens only on Unix sockets for now, so nothing connects here.
    #[derive(Debug)]
    pub struct Listener;

    impl Listener {
        /// Never accepts anyone.
        ///
        /// # Errors
        /// Always.
        pub async fn accept(&self) -> io::Result<(tokio::io::DuplexStream, ())> {
            Err(io::ErrorKind::Unsupported.into())
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
        let dir =
            std::env::temp_dir().join(format!("weacon-testing-listen-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(dir.join("taken")).unwrap();
        let path = dir.join("weacon.sock");
        drop(listen(&path).unwrap());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);
        listen(&path).unwrap();
        listen(&dir.join("taken")).unwrap_err();
        fs::remove_dir_all(&dir).unwrap();
    }
}
