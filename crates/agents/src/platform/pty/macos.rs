//! What macOS names differently from Linux for pseudo-terminals.
#![expect(
    unsafe_code,
    reason = "std has no way to name a pseudo-terminal's other end"
)]

use std::io;

/// Writes the name of the master `fd`'s other end into `name`, ending in NUL.
pub(super) fn name_into(fd: i32, name: &mut [u8; 128]) -> io::Result<()> {
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes, the size of `name`.
    let named = unsafe {
        libc::ioctl(
            fd,
            libc::TIOCPTYGNAME.into(),
            name.as_mut_ptr().cast::<libc::c_char>(),
        )
    };
    if named == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The request that makes a terminal the caller's controlling terminal.
pub(super) fn set_controlling() -> libc::c_ulong {
    libc::TIOCSCTTY.into()
}
