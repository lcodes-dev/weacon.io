//! A vendor's sign-in in a pseudo-terminal, for a person to finish from their browser.

use std::io;

use tokio::sync::mpsc;

use crate::platform::Pty;

/// A terminal's size in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Size {
    /// `None` when either is zero or larger than a console takes.
    #[must_use]
    pub fn new(cols: u32, rows: u32) -> Option<Self> {
        let fits = |count: u32| {
            u16::try_from(count)
                .ok()
                .filter(|count| (1..=MAX_SIDE).contains(count))
        };
        Some(Self {
            cols: fits(cols)?,
            rows: fits(rows)?,
        })
    }
}

const MAX_SIDE: u16 = i16::MAX.cast_unsigned();

/// The vendor's sign-in, running. Dropping it ends the sign-in.
pub struct Terminal {
    pub(crate) pty: Pty,
}

/// What the sign-in does, in order: all it prints, then its end.
pub struct Events(pub(crate) mpsc::Receiver<Event>);

/// One thing the sign-in did.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// What it printed; nothing in Weacon reads it.
    Output(Vec<u8>),
    /// It ended, successfully or not; nothing comes after.
    Exited { success: bool },
}

impl Terminal {
    /// Passes on what the person typed.
    ///
    /// # Errors
    /// When the sign-in can't take it, such as after it ended.
    pub async fn input(&self, data: &[u8]) -> io::Result<()> {
        self.pty.write(data).await
    }

    /// # Errors
    /// When the terminal is gone.
    pub fn resize(&self, size: Size) -> io::Result<()> {
        self.pty.resize(size)
    }
}

impl Events {
    /// The next thing the sign-in did; `None` once it has ended. Safe to cancel.
    pub async fn next(&mut self) -> Option<Event> {
        self.0.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_at_least_one_and_at_most_what_a_console_holds() {
        let size = Size::new(80, 24).unwrap();
        assert_eq!((size.cols, size.rows), (80, 24));
        let largest = Size::new(32_767, 32_767).unwrap();
        assert_eq!((largest.cols, largest.rows), (32_767, 32_767));
        assert!(Size::new(1, 1).is_some());
        for (cols, rows) in [(0, 24), (80, 0), (32_768, 24), (80, 32_768), (70_000, 24)] {
            assert_eq!(Size::new(cols, rows), None, "{cols}x{rows}");
        }
    }

    // A script stands in for each vendor and prints how it was started.
    #[cfg(unix)]
    mod vendors {
        use std::path::Path;

        use weacon_protocol::{AccessLevel, AgentKind};
        use weacon_testing::TempDir;

        use super::*;
        use crate::Launch;
        use crate::test_support::{after_output, next_event, write_program};

        fn launch(agent: AgentKind, dir: &Path) -> Launch {
            let program = dir.join("vendor");
            write_program(
                &program,
                "#!/bin/sh\necho \"args [$*] claude [$CLAUDE_CONFIG_DIR] codex [$CODEX_HOME]\"\n",
            );
            Launch {
                agent,
                program,
                config_dir: dir.join("config"),
                cwd: dir.to_path_buf(),
                access: AccessLevel::AskFirst,
            }
        }

        async fn printed(launch: &Launch) -> (String, bool) {
            let (_terminal, mut events) =
                launch.log_in_terminal(Size::new(80, 24).unwrap()).unwrap();
            let mut seen = String::new();
            match after_output(&mut events.0, &mut seen).await {
                Some(Event::Exited { success }) => (seen, success),
                other => panic!("{other:?} after {seen:?}"),
            }
        }

        #[tokio::test]
        async fn each_vendors_own_login_runs_with_fixed_arguments_and_the_workspace_folder() {
            let dir = TempDir::new().unwrap();
            let config = dir.join("config").display().to_string();
            let (claude, success) = printed(&launch(AgentKind::Claude, &dir)).await;
            assert!(success);
            assert!(
                claude.contains(&format!("args [auth login] claude [{config}] codex []")),
                "{claude}"
            );
            let (codex, _) = printed(&launch(AgentKind::Codex, &dir)).await;
            assert!(
                codex.contains(&format!("args [login] claude [] codex [{config}]")),
                "{codex}"
            );
            launch(AgentKind::Newer, &dir)
                .log_in_terminal(Size::new(80, 24).unwrap())
                .err()
                .unwrap();
        }

        #[tokio::test]
        async fn a_program_that_cant_start_is_an_error_and_ending_the_terminal_ends_its_events() {
            let dir = TempDir::new().unwrap();
            let gone = Launch {
                program: dir.join("missing"),
                ..launch(AgentKind::Claude, &dir)
            };
            let error = gone
                .log_in_terminal(Size::new(80, 24).unwrap())
                .err()
                .unwrap();
            assert_eq!(error.code().as_str(), "agent.login_failed");

            let waiting = dir.join("waiting");
            write_program(&waiting, "#!/bin/sh\necho ready\nread line\n");
            let launch = Launch {
                program: waiting,
                ..launch(AgentKind::Claude, &dir)
            };
            let (terminal, mut events) =
                launch.log_in_terminal(Size::new(80, 24).unwrap()).unwrap();
            let ready = next_event(&mut events.0).await;
            assert!(matches!(ready, Some(Event::Output(_))), "{ready:?}");
            drop(terminal);
            assert_eq!(after_output(&mut events.0, &mut String::new()).await, None);
        }
    }
}
