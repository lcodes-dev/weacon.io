use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(crate) use tokio::net::UnixStream as Stream;

pub(crate) struct Listener(tokio::net::UnixListener);

impl Listener {
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "Windows' listener swaps in a new pipe on each accept"
    )]
    pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
        Ok(self.0.accept().await?.0)
    }
}

/// Takes this process's lock on all of `file`, waiting while another process has it. The lock is
/// the process's own, so a child it forks, even one that hasn't yet run its program, never keeps
/// it, and it ends when the process closes `file`.
#[expect(unsafe_code, reason = "fcntl has no safe API in std")]
pub(crate) fn lock_for_process(file: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: `flock` is plain data, valid all zeros: a lock on the whole file from its start.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::c_short::try_from(libc::F_WRLCK).map_err(|_| io::ErrorKind::Unsupported)?;
    lock.l_whence =
        libc::c_short::try_from(libc::SEEK_SET).map_err(|_| io::ErrorKind::Unsupported)?;
    loop {
        // SAFETY: the descriptor stays open for the call, and `lock` is a valid `flock` it only
        // reads.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLKW, &raw const lock) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Removes `dir` and all it holds; read-only files go too, as the folder's own permission
/// decides.
pub(crate) fn remove_tree(dir: &Path) -> io::Result<()> {
    fs::remove_dir_all(dir)
}

/// Bytes this user may still write on the disk holding `dir`.
#[expect(unsafe_code, reason = "statvfs has no safe API in std")]
#[cfg_attr(
    target_os = "linux",
    expect(clippy::useless_conversion, reason = "f_bavail is u32 on macOS")
)]
pub(crate) fn free_space(dir: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stats` is writable space for one statvfs.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned 0, so it filled `stats`.
    let stats = unsafe { stats.assume_init() };
    Ok(u64::from(stats.f_bavail).saturating_mul(stats.f_frsize))
}

/// Creates `dir` and any missing parents readable only by this user; existing folders keep
/// their permissions.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Lets anyone run `file`, and only its owner change it.
pub(crate) fn make_runnable(file: &Path) -> io::Result<()> {
    fs::set_permissions(file, fs::Permissions::from_mode(0o755))
}

/// Whether `file` is a file this user may run.
#[expect(unsafe_code, reason = "faccessat has no safe API in std")]
pub(crate) fn is_runnable(file: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = CString::new(file.as_os_str().as_bytes()) else {
        return false;
    };
    file.is_file()
        // SAFETY: `path` is NUL-terminated and outlives the call.
        && unsafe {
            libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS)
        } == 0
}

/// Listens on a socket at `path` that only this user can open.
pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
    let listener = tokio::net::UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(Listener(listener))
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn a_lock_the_file_cant_take_is_an_error_not_a_wait() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("list.lock");
        fs::write(&file, "").unwrap();

        let error = lock_for_process(&fs::File::open(&file).unwrap()).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn new_folders_are_private_and_existing_ones_kept() {
        let dir = TempDir::new().unwrap();
        let nested = dir.join("state/logs");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&nested).unwrap();
        private_dir(&dir).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode(&dir), 0o755);
        assert_eq!(mode(&dir.join("state")), 0o700);
        assert_eq!(mode(&nested), 0o700);
    }
}
