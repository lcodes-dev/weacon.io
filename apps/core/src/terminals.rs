//! `weacon terminals`: whether a chat's owner may open their terminal on this machine from the
//! web app. Only a person at this machine can turn it on, so a server's operator never can.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::{App, state_unavailable};
use crate::platform::{self, Env};
use crate::reports::Answer;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    open: bool,
}

fn file(state_dir: &Path) -> PathBuf {
    state_dir.join("terminals.json")
}

/// Whether the person turned it on; off until they do, and while the answer can't be read.
pub(crate) fn allowed(state_dir: &Path) -> bool {
    fs::read(file(state_dir))
        .ok()
        .and_then(|json| serde_json::from_slice::<Stored>(&json).ok())
        .is_some_and(|stored| stored.open)
}

/// Whether a terminal can be opened here from the web app: turned on, and with a desktop.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the worktree command's handler, which offers it, comes next"
    )
)]
pub(crate) fn offered(state_dir: &Path, env: Env<'_>) -> bool {
    allowed(state_dir) && platform::terminal_in(state_dir, env).is_some()
}

fn write(state_dir: &Path, open: bool) -> io::Result<()> {
    platform::private_dir(state_dir)?;
    weacon_appkit::write_whole(&file(state_dir), &serde_json::to_vec(&Stored { open })?)
}

/// Stores `answer` when given, and says what it is now and whether this machine can.
pub(crate) fn run(
    app: &App,
    answer: Option<Answer>,
    env: Env<'_>,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    if let Some(answer) = answer {
        let open = answer == Answer::On;
        write(state_dir, open).map_err(|error| {
            Fault::new(
                code!("terminals.not_saved"),
                "The answer to terminals couldn't be saved",
            )
            .because(error)
        })?;
        app.log.info("terminals chosen", &[("open", open.into())]);
    }
    out.write_all(describe(state_dir, env).as_bytes())
        .map_err(|error| {
            Fault::new(
                code!("terminals.output_failed"),
                "The answer to terminals couldn't be printed",
            )
            .because(error)
        })
}

/// One line for `weacon terminals` and `weacon doctor`.
pub(crate) fn describe(state_dir: &Path, env: Env<'_>) -> &'static str {
    match (
        allowed(state_dir),
        platform::terminal_in(state_dir, env).is_some(),
    ) {
        (true, true) => "Terminals from the web app: on.\n",
        (true, false) => {
            "Terminals from the web app: on, but this machine has no desktop terminal.\n"
        }
        (false, _) => "Terminals from the web app: off. Run weacon terminals on to allow them.\n",
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, env};

    fn terminals(dir: &Path, answer: Option<Answer>, desktop: Env<'_>) -> String {
        let app = test_support::app(Some(dir), None);
        let mut out = Vec::new();
        run(&app, answer, desktop, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn terminals_are_off_until_the_person_here_turns_them_on() {
        let dir = TempDir::new().unwrap();
        let none = env(&[]);
        assert!(!allowed(&dir));
        let off = "Terminals from the web app: off. Run weacon terminals on to allow them.\n";
        assert_eq!(terminals(&dir, None, &none), off);
        let on = terminals(&dir, Some(Answer::On), &none);
        assert!(allowed(&dir));
        assert_eq!(
            on,
            "Terminals from the web app: on, but this machine has no desktop terminal.\n"
        );
        assert!(!offered(&dir, &none));
        assert_eq!(terminals(&dir, Some(Answer::Off), &none), off);
        assert!(!allowed(&dir));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_desktop_with_a_terminal_is_offered_once_turned_on() {
        let dir = TempDir::new().unwrap();
        platform::plant_program(&dir.join("bin"), "xdg-terminal-exec", 0o755);
        let path = dir.join("bin");
        let desktop = env(&[("PATH", path.to_str().unwrap()), ("DISPLAY", ":0")]);
        assert!(!offered(&dir, &desktop));
        let on = terminals(&dir, Some(Answer::On), &desktop);
        assert_eq!(on, "Terminals from the web app: on.\n");
        assert!(offered(&dir, &desktop));
    }

    #[test]
    fn an_answer_that_cant_be_read_is_off() {
        let dir = TempDir::new().unwrap();
        fs::write(file(&dir), "not json").unwrap();
        assert!(!allowed(&dir));
    }

    #[test]
    fn an_answer_that_cant_be_saved_says_so() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(file(&dir)).unwrap();
        let app = test_support::app(Some(&dir), None);
        let fault = run(&app, Some(Answer::On), &env(&[]), &mut Vec::new()).unwrap_err();
        assert_eq!(fault.code().as_str(), "terminals.not_saved");
        let no_state = test_support::app(None, None);
        run(&no_state, None, &env(&[]), &mut Vec::new()).unwrap_err();
    }
}
