//! A program in a pseudo-terminal of its own, driven from elsewhere: its output arrives as
//! events, and its input, size and end are the [`Pty`]'s.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::Pty;
#[cfg(windows)]
pub(crate) use windows::Pty;

/// The most one output event holds.
const CHUNK: usize = 16 * 1024;

#[cfg(test)]
mod tests {
    use tokio::process::Command;
    use tokio::sync::mpsc;
    use weacon_testing::TempDir;

    use super::*;
    use crate::terminal::{Event, Size};
    use crate::test_support::{after_output, next_event};

    // Reads output into `seen` until it holds `wanted`.
    async fn until(events: &mut mpsc::Receiver<Event>, seen: &mut String, wanted: &str) {
        while !seen.contains(wanted) {
            match next_event(events).await {
                Some(Event::Output(data)) => seen.push_str(&String::from_utf8_lossy(&data)),
                other => panic!("{other:?} before {wanted:?}, after {seen:?}"),
            }
        }
    }

    #[cfg(unix)]
    fn program(dir: &TempDir) -> Command {
        let path = dir.join("terminal-check");
        crate::test_support::write_program(
            &path,
            "#!/bin/sh\n\
             [ -t 0 ] && [ -t 1 ] && echo 'stdin and stdout are a terminal'\n\
             stty size\n\
             IFS= read -r line\n\
             echo \"got $line\"\n\
             IFS= read -r line\n\
             stty size\n\
             exit 3\n",
        );
        Command::new(path)
    }

    #[cfg(windows)]
    fn program(_dir: &TempDir) -> Command {
        let root = std::env::var_os("SystemRoot").unwrap();
        let shell =
            std::path::Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut command = Command::new(shell);
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "if (-not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected) \
             { 'stdin and stdout are a terminal' }; \
             '{0} {1}' -f [Console]::WindowHeight, [Console]::WindowWidth; \
             'got ' + [Console]::ReadLine(); \
             $null = [Console]::ReadLine(); \
             '{0} {1}' -f [Console]::WindowHeight, [Console]::WindowWidth; \
             exit 3",
        ]);
        command
    }

    #[tokio::test]
    async fn a_program_gets_a_terminal_of_its_size_that_follows_a_resize() {
        let dir = TempDir::new().unwrap();
        let (pty, mut events) = Pty::spawn(program(&dir), Size::new(100, 30).unwrap()).unwrap();
        let mut seen = String::new();
        until(&mut events, &mut seen, "stdin and stdout are a terminal").await;
        until(&mut events, &mut seen, "30 100").await;
        pty.write(b"MARKER-7f3a\r").await.unwrap();
        until(&mut events, &mut seen, "got MARKER-7f3a").await;
        pty.resize(Size::new(120, 40).unwrap()).unwrap();
        pty.write(b"\r").await.unwrap();
        until(&mut events, &mut seen, "40 120").await;
        let end = after_output(&mut events, &mut seen).await;
        assert_eq!(end, Some(Event::Exited { success: false }));
        assert_eq!(next_event(&mut events).await, None);
    }
}
