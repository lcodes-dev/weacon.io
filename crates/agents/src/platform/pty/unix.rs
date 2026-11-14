#![expect(
    unsafe_code,
    reason = "std has no pseudo-terminals: they need grantpt, unlockpt, ptsname and ioctl"
)]

use std::ffi::{CStr, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::CHUNK;
use crate::terminal::{Event, Size};

/// The program's side ends when it does; dropping this kills it.
pub(crate) struct Pty {
    master: Arc<AsyncFd<File>>,
    task: JoinHandle<()>,
}

impl Pty {
    /// Starts `command` with a new pseudo-terminal of `size` as its controlling terminal and
    /// stdin, stdout and stderr, in a session of its own.
    pub(crate) fn spawn(
        mut command: Command,
        size: Size,
    ) -> io::Result<(Self, mpsc::Receiver<Event>)> {
        let (master, slave) = open_pair()?;
        set_size(&master, size)?;
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .kill_on_drop(true);
        // SAFETY: the closure only calls `setsid` and `ioctl`, which are async-signal-safe,
        // and allocates nothing.
        unsafe { command.pre_exec(take_terminal) };
        let child = command.spawn()?;
        // The parent's copies of the program's side go, so the master sees its end.
        drop(command);
        // SAFETY: the file owns the master, and nothing closes or replaces it while it is
        // registered.
        let master = Arc::new(unsafe { AsyncFd::register(File::from(master))? });
        let (events, received) = mpsc::channel(8);
        let task = tokio::spawn(pump(Arc::clone(&master), child, events));
        Ok((Self { master, task }, received))
    }

    /// Writes all of `data` as if typed.
    pub(crate) async fn write(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let mut ready = self.master.writable().await?;
            if let Ok(written) = ready.try_io(|master| master.get_ref().write(data)) {
                data = data.get(written?..).unwrap_or_default();
            }
        }
        Ok(())
    }

    pub(crate) fn resize(&self, size: Size) -> io::Result<()> {
        set_size(self.master.get_ref(), size)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// Sends the output until the program's side closes, then how the program ended.
async fn pump(master: Arc<AsyncFd<File>>, mut child: Child, events: mpsc::Sender<Event>) {
    let mut buffer = vec![0; CHUNK];
    loop {
        let Ok(mut ready) = master.readable().await else {
            break;
        };
        match ready.try_io(|master| master.get_ref().read(&mut buffer)) {
            Err(_would_block) => {}
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
            // Linux reports the closed side as an error, macOS as the end.
            Ok(Ok(0) | Err(_)) => break,
            Ok(Ok(read)) => {
                let chunk = buffer.get(..read).unwrap_or_default().to_vec();
                if events.send(Event::Output(chunk)).await.is_err() {
                    return;
                }
            }
        }
    }
    let success = child.wait().await.is_ok_and(|status| status.success());
    drop(events.send(Event::Exited { success }).await);
}

// Runs in the child between fork and exec, after its stdio is the terminal.
fn take_terminal() -> io::Result<()> {
    // SAFETY: plain system calls on this process and its stdin.
    let taken = unsafe { libc::setsid() != -1 && libc::ioctl(0, set_controlling(), 0) != -1 };
    if taken {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// Both ends of a new pseudo-terminal, close-on-exec as std opens every file, the master
// non-blocking.
fn open_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let master = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open("/dev/ptmx")?;
    let fd = master.as_raw_fd();
    // SAFETY: `fd` is the open master of a pseudo-terminal.
    if unsafe { libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 } {
        return Err(io::Error::last_os_error());
    }
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(slave_name(fd)?)?;
    Ok((master.into(), slave.into()))
}

fn set_size(master: &impl AsRawFd, size: Size) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the master is open, and `size` outlives the call.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// The path of the master's other end.
fn slave_name(fd: i32) -> io::Result<OsString> {
    let mut name = [0; 128];
    name_into(fd, &mut name)?;
    let name = CStr::from_bytes_until_nul(&name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "no pseudo-terminal name"))?;
    Ok(OsStr::from_bytes(name.to_bytes()).to_owned())
}

#[cfg(target_os = "macos")]
fn name_into(fd: i32, name: &mut [u8; 128]) -> io::Result<()> {
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

#[cfg(not(target_os = "macos"))]
fn name_into(fd: i32, name: &mut [u8; 128]) -> io::Result<()> {
    // SAFETY: `ptsname_r` writes at most `name.len()` bytes into `name`.
    let failed = unsafe { libc::ptsname_r(fd, name.as_mut_ptr().cast(), name.len()) };
    if failed != 0 {
        return Err(io::Error::from_raw_os_error(failed));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_controlling() -> libc::c_ulong {
    libc::TIOCSCTTY.into()
}

#[cfg(not(target_os = "macos"))]
const fn set_controlling() -> libc::Ioctl {
    libc::TIOCSCTTY
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(fd: &OwnedFd, which: i32) -> i32 {
        // SAFETY: `fd` is open for the call.
        unsafe { libc::fcntl(fd.as_raw_fd(), which) }
    }

    #[test]
    fn both_ends_close_on_exec_and_only_the_master_never_blocks() {
        let (master, slave) = open_pair().unwrap();
        for end in [&master, &slave] {
            assert_ne!(flags(end, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
        }
        assert_ne!(flags(&master, libc::F_GETFL) & libc::O_NONBLOCK, 0);
        assert_eq!(flags(&slave, libc::F_GETFL) & libc::O_NONBLOCK, 0);
    }
}
