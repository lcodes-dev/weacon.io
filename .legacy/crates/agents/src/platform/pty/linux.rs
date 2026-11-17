//! What Linux names differently from macOS for pseudo-terminals.
#![expect(
    unsafe_code,
    reason = "std has no way to name a pseudo-terminal's other end"
)]

use std::io;

/// Writes the name of the master `fd`'s other end into `name`, ending in NUL.
pub(super) fn name_into(fd: i32, name: &mut [u8; 128]) -> io::Result<()> {
    // SAFETY: `ptsname_r` writes at most `name.len()` bytes into `name`.
    let failed = unsafe { libc::ptsname_r(fd, name.as_mut_ptr().cast(), name.len()) };
    if failed != 0 {
        return Err(io::Error::from_raw_os_error(failed));
    }
    Ok(())
}

/// The request that makes a terminal the caller's controlling terminal.
pub(super) const fn set_controlling() -> libc::Ioctl {
    libc::TIOCSCTTY
}
