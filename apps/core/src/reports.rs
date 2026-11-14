//! The person's answer to whether this core sends error reports, kept in the state folder.

use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use weacon_observability::{Fault, Field, Logger, Reporter};
use weacon_protocol::code;

use crate::app::{App, state_unavailable};
use crate::platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Answer {
    On,
    Off,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    send: bool,
}

fn file(state_dir: &Path) -> PathBuf {
    state_dir.join("reports.json")
}

/// Whether the stored answer is yes; nothing is sent while it can't be read.
pub(crate) fn agreed(state_dir: &Path, log: &Logger) -> bool {
    read(state_dir).unwrap_or_else(|error| {
        log.warn("reports unreadable", &[("err", Field::error(&error))]);
        None
    }) == Some(true)
}

const CHECK: Duration = Duration::from_secs(10);

/// Runs `work` while keeping `reporter` in step with the answer stored in `state_dir`, so a
/// command that runs for long stops sending as soon as the person says no.
pub(crate) async fn following<T>(
    state_dir: &Path,
    reporter: &Arc<Reporter>,
    log: &Logger,
    work: impl Future<Output = T>,
) -> T {
    tokio::select! {
        done = work => done,
        never = Watch::new(state_dir.to_owned()).follow(reporter, log) => match never {},
    }
}

struct Watch {
    state_dir: PathBuf,
    failing: bool,
}

impl Watch {
    const fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            failing: false,
        }
    }

    /// Checks the answer every few seconds, forever.
    async fn follow(mut self, reporter: &Arc<Reporter>, log: &Logger) -> ! {
        let mut tick = tokio::time::interval(CHECK);
        loop {
            tick.tick().await;
            self.check(reporter, log).await;
        }
    }

    /// Reads the answer off the async runtime; one that can't be read sends nothing, and is
    /// logged once until a reading succeeds.
    async fn check(&mut self, reporter: &Arc<Reporter>, log: &Logger) {
        let (dir, following) = (self.state_dir.clone(), Arc::clone(reporter));
        // A read that never ran applied nothing, so the reporter keeps the answer applied last.
        let read = tokio::task::spawn_blocking(move || applied(&dir, &following))
            .await
            .unwrap_or_else(|error| Err(unreadable().because(error)));
        self.observe(read.map(drop), log);
    }

    // Only logs: the reporter was brought into line under the lock, and a change saved since
    // must stand.
    fn observe(&mut self, read: Result<(), Fault>, log: &Logger) {
        match read {
            Ok(()) => self.failing = false,
            Err(error) => {
                if !self.failing {
                    log.warn("reports unreadable", &[("err", Field::error(&error))]);
                }
                self.failing = true;
            }
        }
    }
}

// Reads the saved answer and applies it, as one step; one that can't be read sends nothing.
fn applied(state_dir: &Path, reporter: &Reporter) -> Result<Option<bool>, Fault> {
    settled(|| {
        let read = read(state_dir);
        reporter.set_agreed(matches!(read, Ok(Some(true))));
        read
    })
}

fn unreadable() -> Fault {
    Fault::new(
        code!("reports.unreadable"),
        "The answer to error reports couldn't be read",
    )
}

// `None` until the person answers; a state folder that is a file holds no answer either.
fn read(state_dir: &Path) -> Result<Option<bool>, Fault> {
    match fs::read(file(state_dir)) {
        Ok(json) => serde_json::from_slice::<Stored>(&json)
            .map(|stored| Some(stored.send))
            .map_err(|error| unreadable().because(error)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(unreadable().because(error)),
    }
}

fn write(state_dir: &Path, send: bool) -> io::Result<()> {
    platform::private_dir(state_dir)?;
    weacon_appkit::write_whole(&file(state_dir), &serde_json::to_vec(&Stored { send })?)
}

/// The stored answer; one that can't be read counts as none, as `App::start` warned.
pub(crate) fn current(state_dir: &Path) -> Option<bool> {
    read(state_dir).unwrap_or(None)
}

/// Stores the person's answer and applies it to `reporter` at once.
pub(crate) fn store(
    state_dir: &Path,
    send: bool,
    reporter: &Reporter,
    log: &Logger,
) -> Result<(), Fault> {
    saved(state_dir, send, |send| reporter.set_agreed(send))?;
    log.info("reports chosen", &[("send", send.into())]);
    Ok(())
}

// Saves the answer, then hands it to `agree`, as one step: the answer a reporter follows is
// always the one saved last, and one that couldn't be saved changes nothing.
fn saved(state_dir: &Path, send: bool, agree: impl FnOnce(bool)) -> Result<(), Fault> {
    settled(|| {
        write(state_dir, send).map_err(|error| {
            Fault::new(
                code!("reports.not_saved"),
                "The answer to error reports couldn't be saved",
            )
            .because(error)
        })?;
        agree(send);
        Ok(())
    })
}

// Held while the saved answer and a reporter are brought into line, by a change or by the watch.
static SETTLING: Mutex<()> = Mutex::new(());

fn settled<T>(step: impl FnOnce() -> T) -> T {
    let _held = SETTLING.lock().unwrap_or_else(PoisonError::into_inner);
    step()
}

/// Stores `answer` when given, applies it to this run, and says what the answer is now.
pub(crate) fn run(app: &App, answer: Option<Answer>, out: &mut impl Write) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let send = match answer {
        Some(answer) => {
            let send = answer == Answer::On;
            store(state_dir, send, &app.reporter, &app.log)?;
            Some(send)
        }
        None => current(state_dir),
    };
    out.write_all(describe(send).as_bytes()).map_err(|error| {
        Fault::new(
            code!("reports.output_failed"),
            "The answer to error reports couldn't be printed",
        )
        .because(error)
    })
}

fn describe(send: Option<bool>) -> &'static str {
    match send {
        Some(true) => "Error reports: on. They hold no messages, prompts, code or file contents.\n",
        Some(false) => "Error reports: off.\n",
        None => "Error reports: off until you choose. Run weacon reports on, or off.\n",
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, Broken};

    fn reports(dir: &Path, answer: Option<Answer>) -> Result<String, Fault> {
        let app = test_support::app(Some(dir), None);
        let mut out = Vec::new();
        run(&app, answer, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_change_saved_after_a_failed_read_stands() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);
        let (log, records) = test_support::recording_logger();
        fs::write(file(&dir), "not json").unwrap();
        let mut watch = Watch::new(dir.to_path_buf());
        app.reporter.set_agreed(true);

        let read = applied(&dir, &app.reporter);
        assert!(!app.reporter.agreed());
        store(&dir, true, &app.reporter, &log).unwrap();
        watch.observe(read.map(drop), &log);

        assert!(app.reporter.agreed());
        assert_eq!(current(&dir), Some(true));
        assert_eq!(
            test_support::messages(&records),
            ["reports chosen", "reports unreadable"]
        );
    }

    #[test]
    fn an_answer_is_saved_and_applied_as_one_step_and_one_not_saved_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let mut applied = None;
        saved(&dir, true, |send| {
            // Nothing else can save or read an answer until this one is applied.
            assert!(SETTLING.try_lock().is_err(), "applied outside the lock");
            assert_eq!(current(&dir), Some(true));
            applied = Some(send);
        })
        .unwrap();
        let blocked = TempDir::new().unwrap();
        fs::create_dir_all(file(&blocked)).unwrap();
        let mut reached = false;
        let unsaved = saved(&blocked, true, |_| reached = true).unwrap_err();

        assert_eq!(applied, Some(true));
        assert_eq!(unsaved.code().as_str(), "reports.not_saved");
        assert!(!reached);
    }

    #[test]
    fn nothing_is_chosen_until_the_person_answers_and_the_answer_is_kept() {
        let dir = TempDir::new().unwrap();
        let before = reports(&dir, None).unwrap();
        let on = reports(&dir, Some(Answer::On)).unwrap();
        let kept_on = reports(&dir, None).unwrap();
        let stored_on = fs::read_to_string(file(&dir)).unwrap();
        let off = reports(&dir, Some(Answer::Off)).unwrap();

        assert_eq!(
            before,
            "Error reports: off until you choose. Run weacon reports on, or off.\n"
        );
        assert!(
            on.starts_with("Error reports: on. They hold no messages"),
            "{on}"
        );
        assert_eq!(kept_on, on);
        assert_eq!(stored_on, r#"{"send":true}"#);
        assert_eq!(off, "Error reports: off.\n");
        assert_eq!(read(&dir).unwrap(), Some(false));
        assert_eq!(reports(&dir, None).unwrap(), off);
    }

    #[test]
    fn an_answer_applies_to_the_run_that_gives_it() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);
        app.reporter.set_agreed(false);

        run(&app, Some(Answer::On), &mut Vec::new()).unwrap();
        assert!(app.reporter.agreed());
        run(&app, Some(Answer::Off), &mut Vec::new()).unwrap();
        assert!(!app.reporter.agreed());
        run(&app, None, &mut Vec::new()).unwrap();
        assert!(!app.reporter.agreed());
    }

    #[test]
    fn a_broken_or_newer_file_still_reads_or_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::write(file(&dir), r#"{"send":true,"later":1}"#).unwrap();
        assert_eq!(read(&dir).unwrap(), Some(true));
        fs::write(file(&dir), "not json").unwrap();
        let broken = read(&dir).unwrap_err();
        let shown = reports(&dir, None).unwrap();
        let not_a_folder = dir.join("reports.json");
        let replaced = reports(&dir, Some(Answer::Off)).unwrap();

        assert_eq!(broken.code().as_str(), "reports.unreadable");
        assert!(
            shown.starts_with("Error reports: off until you choose."),
            "{shown}"
        );
        assert_eq!(read(&not_a_folder).unwrap(), None);
        let folder = TempDir::new().unwrap();
        fs::create_dir_all(file(&folder)).unwrap();
        assert_eq!(
            read(&folder).unwrap_err().code().as_str(),
            "reports.unreadable"
        );
        assert!(replaced.starts_with("Error reports: off."), "{replaced}");
        assert_eq!(read(&dir).unwrap(), Some(false));
    }

    #[tokio::test]
    async fn a_watch_follows_the_stored_answer_and_warns_once_while_it_cant_be_read() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);
        let (log, records) = test_support::recording_logger();
        let mut watch = Watch::new(dir.to_path_buf());
        app.reporter.set_agreed(true);
        watch.check(&app.reporter, &log).await;
        let mut agreed = vec![app.reporter.agreed()];
        for answer in [r#"{"send":true}"#, "not json", "still not"] {
            fs::write(file(&dir), answer).unwrap();
            app.reporter.set_agreed(true);
            watch.check(&app.reporter, &log).await;
            agreed.push(app.reporter.agreed());
        }
        let broken = test_support::messages(&records);
        fs::write(file(&dir), r#"{"send":false}"#).unwrap();
        watch.check(&app.reporter, &log).await;
        fs::write(file(&dir), "broken again").unwrap();
        watch.check(&app.reporter, &log).await;

        assert_eq!(agreed, [false, true, false, false]);
        assert_eq!(broken, ["reports unreadable"]);
        assert!(!app.reporter.agreed());
        assert_eq!(
            test_support::messages(&records),
            ["reports unreadable", "reports unreadable"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn long_work_stops_sending_once_the_person_says_no() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);
        let (log, _) = test_support::recording_logger();
        fs::write(file(&dir), r#"{"send":true}"#).unwrap();
        let work = async {
            tokio::time::sleep(CHECK).await;
            let before = app.reporter.agreed();
            fs::write(file(&dir), r#"{"send":false}"#).unwrap();
            tokio::time::sleep(CHECK * 2).await;
            (before, app.reporter.agreed())
        };

        let seen = following(&dir, &app.reporter, &log, work).await;

        assert_eq!(seen, (true, false));
    }

    #[test]
    fn an_answer_that_cant_be_saved_or_printed_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(file(&dir)).unwrap();
        let app = test_support::app(Some(&dir), None);
        app.reporter.set_agreed(false);
        let unsaved = run(&app, Some(Answer::On), &mut Vec::new()).unwrap_err();
        let printable = TempDir::new().unwrap();
        let unprinted = run(
            &test_support::app(Some(&printable), None),
            Some(Answer::On),
            &mut Broken,
        )
        .unwrap_err();
        let nowhere = run(&test_support::app(None, None), None, &mut Vec::new()).unwrap_err();

        assert_eq!(unsaved.code().as_str(), "reports.not_saved");
        assert!(!app.reporter.agreed());
        assert_eq!(unprinted.code().as_str(), "reports.output_failed");
        assert_eq!(read(&printable).unwrap(), Some(true));
        assert_eq!(nowhere.code().as_str(), "state.unavailable");
    }
}
