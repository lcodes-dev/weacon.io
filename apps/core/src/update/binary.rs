//! What a weacon binary answers when asked, within a deadline.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use weacon_protocol::Commit;

/// How often, and how far apart, a program that is busy being written is tried again.
const BUSY_TRIES: u32 = 50;
const BUSY_PAUSE: Duration = Duration::from_millis(10);

/// How long a binary has to answer `--version` or `status` before it is killed.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

pub(super) fn version_of(binary: &Path) -> io::Result<String> {
    output_within(Command::new(binary).arg("--version"), ANSWER_WITHIN)
}

/// What `command` prints, up to 64 KiB, once it ends; it is killed if it takes over `within`.
/// Its output must end within that time too, even when a process it started holds it open.
pub(super) fn output_within(command: &mut Command, within: Duration) -> io::Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = when_free(|| command.spawn())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let (sent, printed) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        drop(
            sent.send(
                stdout
                    .take(64 * 1024)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes),
            ),
        );
    });
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() >= within {
            drop(child.kill());
            child.wait()?;
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let left = within.saturating_sub(started.elapsed());
    let bytes = printed
        .recv_timeout(left)
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What `spawn` gives once the program isn't busy. A process another thread forked holds a file
/// just written open until it starts its own program, as when an agent starts beside an update.
fn when_free<T>(mut spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    for _ in 1..BUSY_TRIES {
        match spawn() {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(BUSY_PAUSE);
            }
            spawned => return spawned,
        }
    }
    spawn()
}

/// Asks `status` of the binary at `exe` up to `tries` times, `pause` apart, until it names
/// `commit`. The new binary asks, so it speaks the restarted daemon's protocol even when new.
pub(super) fn status_names(exe: &Path, commit: &Commit, tries: u32, pause: Duration) -> bool {
    let wanted = format!("({})", commit.as_str());
    (0..tries).any(|_| {
        std::thread::sleep(pause);
        output_within(Command::new(exe).arg("status"), ANSWER_WITHIN)
            .is_ok_and(|printed| printed.contains(&wanted))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use weacon_testing::TempDir;

    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[cfg(unix)]
    #[test]
    fn a_binary_is_asked_for_its_version() {
        let dir = TempDir::new().unwrap();
        let binary = dir.join("weacon");
        fs::write(&binary, "#!/bin/sh\necho \"asked $1\"\n").unwrap();
        crate::platform::make_runnable(&binary).unwrap();
        assert_eq!(version_of(&binary).unwrap(), "asked --version\n");
        version_of(&dir.join("missing")).unwrap_err();
    }

    /// A binary in `dir` that answers every command with `line`.
    #[cfg(unix)]
    fn answering(dir: &Path, line: &str) -> PathBuf {
        let binary = dir.join("weacon");
        fs::write(&binary, format!("#!/bin/sh\necho \"{line}\"\n")).unwrap();
        crate::platform::make_runnable(&binary).unwrap();
        binary
    }

    #[cfg(unix)]
    #[test]
    fn a_restarted_daemon_counts_once_its_status_names_the_new_commit() {
        let dir = TempDir::new().unwrap();
        let commit = Commit::try_from(COMMIT.to_owned()).unwrap();
        let new = answering(&dir, &format!("Daemon: running, weacon 1.2.3 ({COMMIT})"));
        assert!(status_names(&new, &commit, 1, Duration::ZERO));
        assert!(!status_names(&new, &commit, 0, Duration::ZERO));
        let old = answering(&dir, "Daemon: running, weacon 1.0.0 (1a2b3c4)");
        assert!(!status_names(&old, &commit, 3, Duration::ZERO));
        assert!(!status_names(
            &dir.join("missing"),
            &commit,
            1,
            Duration::ZERO
        ));
    }

    #[test]
    fn a_program_busy_being_written_is_started_once_it_is_free() {
        let busy = || io::Error::from(io::ErrorKind::ExecutableFileBusy);
        let mut tries = 0;
        let started = when_free(|| {
            tries += 1;
            if tries < 3 { Err(busy()) } else { Ok(tries) }
        });
        assert_eq!(started.unwrap(), 3);

        let mut tries = 0;
        let never = when_free(|| -> io::Result<()> {
            tries += 1;
            Err(busy())
        });
        assert_eq!(never.unwrap_err().kind(), io::ErrorKind::ExecutableFileBusy);
        assert_eq!(tries, BUSY_TRIES);
        let mut tries = 0;
        let other = when_free(|| -> io::Result<()> {
            tries += 1;
            Err(io::ErrorKind::NotFound.into())
        });
        assert_eq!(
            (other.unwrap_err().kind(), tries),
            (io::ErrorKind::NotFound, 1)
        );
    }

    // `answer`'s result, failing the test rather than hanging it when it doesn't come in time.
    #[cfg(unix)]
    fn soon<T: Send + 'static>(answer: impl FnOnce() -> T + Send + 'static) -> T {
        let (sent, answered) = mpsc::channel();
        std::thread::spawn(move || drop(sent.send(answer())));
        answered.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_that_hangs_is_killed() {
        let dir = TempDir::new().unwrap();
        let hangs = dir.join("hangs");
        // Far longer than the test waits, so only the deadline can end it in time.
        fs::write(&hangs, "#!/bin/sh\nexec sleep 60\n").unwrap();
        crate::platform::make_runnable(&hangs).unwrap();
        let error = soon(move || output_within(&mut Command::new(&hangs), Duration::ZERO));
        let error = error.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let fast = answering(&dir, "quick");
        let printed = output_within(&mut Command::new(&fast), Duration::from_secs(30)).unwrap();
        assert_eq!(printed, "quick\n");
        let long = answering(&dir, &"x".repeat(5000));
        let printed = output_within(&mut Command::new(&long), Duration::from_secs(30)).unwrap();
        assert_eq!(printed.len(), 5001);
    }

    #[cfg(unix)]
    #[test]
    fn a_process_left_holding_the_output_doesnt_hold_the_answer_past_its_time() {
        let dir = TempDir::new().unwrap();
        let leaves = dir.join("leaves");
        let left = dir.join("left");
        let script = format!(
            "#!/bin/sh\necho started\nsleep 60 &\necho $! > '{}'\n",
            left.display()
        );
        fs::write(&leaves, script).unwrap();
        crate::platform::make_runnable(&leaves).unwrap();
        let answer =
            soon(move || output_within(&mut Command::new(&leaves), Duration::from_millis(500)));
        let error = answer.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let pid = fs::read_to_string(&left).unwrap();
        let killed = Command::new("kill").arg(pid.trim()).status().unwrap();
        assert!(killed.success());
    }
}
