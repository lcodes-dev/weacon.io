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
#[cfg(not(target_os = "macos"))]
use super::linux::{name_into, set_controlling};
#[cfg(target_os = "macos")]
use super::macos::{name_into, set_controlling};
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

// Sends the output, then how the program ended. The end comes when the program ends, even if
// something it started still holds its side open.
async fn pump(master: Arc<AsyncFd<File>>, mut child: Child, events: mpsc::Sender<Event>) {
    let mut buffer = vec![0; CHUNK];
    let success = loop {
        let read = tokio::select! {
            status = child.wait() => break status.is_ok_and(|status| status.success()),
            read = read(&master, &mut buffer) => read,
        };
        match read {
            Reading::Later => {}
            Reading::End => break child.wait().await.is_ok_and(|status| status.success()),
            Reading::Data(read) => {
                if !send(&events, &buffer, read).await {
                    return;
                }
            }
        }
    };
    // What the program wrote just before it ended is still to be read, but no more than
    // that: something it left running may never stop printing.
    for _ in 0..DRAINED {
        match master.get_ref().read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let chunk = buffer.get(..read).unwrap_or_default().to_vec();
                drop(events.send(Event::Output(chunk)).await);
            }
        }
    }
    drop(events.send(Event::Exited { success }).await);
}

const DRAINED: usize = 4;

enum Reading {
    Data(usize),
    Later,
    End,
}

async fn read(master: &AsyncFd<File>, buffer: &mut [u8]) -> Reading {
    let Ok(mut ready) = master.readable().await else {
        return Reading::End;
    };
    match ready.try_io(|master| master.get_ref().read(buffer)) {
        Err(_would_block) => Reading::Later,
        // Linux reports the closed side as an error, macOS as the end.
        Ok(Ok(0) | Err(_)) => Reading::End,
        Ok(Ok(read)) => Reading::Data(read),
    }
}

// Whether the receiver still listens.
async fn send(events: &mpsc::Sender<Event>, buffer: &[u8], read: usize) -> bool {
    let chunk = buffer.get(..read).unwrap_or_default().to_vec();
    events.send(Event::Output(chunk)).await.is_ok()
}

// Runs in the child between fork and exec, after its stdio is the terminal.
fn take_terminal() -> io::Result<()> {
    // SAFETY: plain system calls on this process and its stdin.
    checked(unsafe { libc::setsid() })?;
    // SAFETY: as above.
    checked(unsafe { libc::ioctl(0, set_controlling(), 0) })?;
    Ok(())
}

// A system call's result, or the error it set; allocates nothing.
fn checked(result: i32) -> io::Result<i32> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
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
    checked(unsafe { libc::grantpt(fd) })?;
    // SAFETY: as above.
    checked(unsafe { libc::unlockpt(fd) })?;
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
    checked(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(fd: &OwnedFd, which: i32) -> i32 {
        // SAFETY: `fd` is open for the call.
        unsafe { libc::fcntl(fd.as_raw_fd(), which) }
    }

    #[test]
    fn a_file_that_isnt_a_terminal_is_an_error() {
        let dir = weacon_testing::TempDir::new().unwrap();
        let file = File::create(dir.join("plain")).unwrap();
        set_size(&file, Size::new(80, 24).unwrap()).unwrap_err();
        name_into(file.as_raw_fd(), &mut [0; 128]).unwrap_err();
        assert_eq!(checked(7).unwrap(), 7);
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
