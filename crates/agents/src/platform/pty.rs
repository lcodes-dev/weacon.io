//! A program in a pseudo-terminal of its own, driven from elsewhere: its output arrives as
//! events, and its input, size and end are the [`Pty`]'s.

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::Pty;
#[cfg(windows)]
pub(crate) use windows::Pty;

/// The most one output event holds.
const CHUNK: usize = 16_384;

#[cfg(test)]
mod tests {
    use tokio::process::Command;
    use weacon_testing::TempDir;

    use super::*;
    use crate::terminal::{Event, Events, Size};
    use crate::test_support::{after_output, next_event, output_until};

    #[cfg(unix)]
    fn program(dir: &TempDir) -> Command {
        let path = dir.join("terminal-check");
        crate::platform::write_program(
            &path,
            "#!/bin/sh\n\
             [ -t 0 ] && [ -t 1 ] && [ \"$(stty size < /dev/tty)\" = \"$(stty size)\" ] && \
             echo 'stdin and stdout are its controlling terminal'\n\
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
             { 'stdin and stdout are its controlling terminal' }; \
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
        let (pty, events) = Pty::spawn(program(&dir), Size::new(100, 30).unwrap()).unwrap();
        let mut events = Events(events);
        let mut seen = String::new();
        output_until(
            &mut events,
            &mut seen,
            "stdin and stdout are its controlling terminal",
        )
        .await;
        output_until(&mut events, &mut seen, "30 100").await;
        pty.write(b"MARKER-7f3a\r").await.unwrap();
        output_until(&mut events, &mut seen, "got MARKER-7f3a").await;
        pty.resize(Size::new(120, 40).unwrap()).unwrap();
        pty.write(b"\r").await.unwrap();
        output_until(&mut events, &mut seen, "40 120").await;
        let end = after_output(&mut events, &mut seen).await;
        assert_eq!(end, Some(Event::Exited { success: false }));
        assert_eq!(next_event(&mut events).await, None);
    }

    // Something the program starts may keep the terminal open after the program ends.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_comes_when_the_program_ends_whatever_it_left_running() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("leaves");
        crate::platform::write_program(
            &path,
            "#!/bin/sh\n(trap '' HUP; exec sleep 30) &\necho left\nexit 0\n",
        );
        let (_pty, events) = Pty::spawn(Command::new(path), Size::new(80, 24).unwrap()).unwrap();
        let mut events = Events(events);
        let mut seen = String::new();
        let end = after_output(&mut events, &mut seen).await;
        assert_eq!(end, Some(Event::Exited { success: true }));
        assert!(seen.contains("left"), "{seen:?}");
    }
}
