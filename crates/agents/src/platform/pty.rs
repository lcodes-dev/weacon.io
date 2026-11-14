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
        weacon_testing::platform::write_program(
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
        )
        .unwrap();
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

    // Something the program starts may keep the terminal open after the program ends. Here it
    // waits on the terminal, so it ends as soon as the terminal does, with the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_comes_when_the_program_ends_whatever_it_left_running() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("leaves");
        weacon_testing::platform::write_program(
            &path,
            "#!/bin/sh\n(trap '' HUP; read -r line < /dev/tty) &\necho left\nexit 0\n",
        )
        .unwrap();
        let (pty, events) = Pty::spawn(Command::new(path), Size::new(80, 24).unwrap()).unwrap();
        let mut events = Events(events);
        let mut seen = String::new();
        let end = after_output(&mut events, &mut seen).await;
        assert_eq!(end, Some(Event::Exited { success: true }));
        assert!(seen.contains("left"), "{seen:?}");
        drop(pty);
    }

    // A terminal printing without end, dropped once its events fill up unread.
    #[cfg(windows)]
    async fn dropped_unread() -> tokio::sync::mpsc::Receiver<Event> {
        let root = std::env::var_os("SystemRoot").unwrap();
        let shell =
            std::path::Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut chatty = Command::new(shell);
        chatty.args([
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "1..100000 | % { 'x' * 70 }",
        ]);
        let (pty, events) = Pty::spawn(chatty, Size::new(80, 24).unwrap()).unwrap();
        let full = std::time::Duration::from_mins(1);
        tokio::time::timeout(full, until_full(&events))
            .await
            .unwrap();
        drop(pty);
        events
    }

    #[cfg(windows)]
    async fn until_full(events: &tokio::sync::mpsc::Receiver<Event>) {
        while events.capacity() > 0 {
            tokio::task::yield_now().await;
        }
    }

    // Unread events once blocked the thread that reads the console, and with it the runtime's end.
    #[cfg(windows)]
    #[test]
    fn dropping_the_terminal_lets_the_runtime_stop_though_its_events_go_unread() {
        use std::time::{Duration, Instant};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let unread = runtime.block_on(dropped_unread());
        let started = Instant::now();
        runtime.shutdown_timeout(Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(30));
        drop(unread);
    }
}
