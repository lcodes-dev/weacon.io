use std::ffi::OsString;
use std::io;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use weacon_agents::SpawnPolicy;
use weacon_appkit::{KEEP, MAX_BYTES, RotatingFile, panic_hook, root_logger};
use weacon_observability::{
    Deliver, Fault, Field, Level, Logger, Reporter, Reports, Sink, json_lines, tee,
};
use weacon_protocol::code;
use weacon_version::Version;

use crate::platform::{self, Env};

use weacon_appkit::Outcome;

/// What a command runs with: its build, logger, error reporter and where its state lives.
pub(crate) struct App {
    pub(crate) version: Version,
    pub(crate) log: Logger,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) state_dir: Option<PathBuf>,
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) spawn: SpawnPolicy,
    /// The person's `PATH`, where their agents' programs are found.
    pub(crate) path: Option<OsString>,
}

/// The outside world an [`App`] starts from; `main` passes the real one.
pub(crate) struct Setup<'a> {
    pub(crate) env: Env<'a>,
    pub(crate) stderr: Sink,
    pub(crate) deliver: Deliver,
    pub(crate) verbose: bool,
    pub(crate) mode: &'static str,
    /// The log file's name: the daemon and short commands each rotate their own.
    pub(crate) log_name: &'static str,
}

impl App {
    pub(crate) fn start(version: Version, setup: Setup<'_>) -> Self {
        let state_dir = platform::state_dir(setup.env);
        let (log_file, file_sink, failure) = match state_dir
            .as_deref()
            .map(|dir| open_log(dir, setup.log_name))
        {
            Some(Ok((path, file))) => (Some(path), json_lines(file), None),
            Some(Err(error)) => (None, nothing(), Some(error)),
            None => (None, nothing(), None),
        };
        let stderr_from = if setup.verbose {
            Level::Info
        } else {
            Level::Warn
        };
        let log = root_logger(tee(file_sink, setup.stderr, stderr_from), &version);
        log.info("app started", &[("mode", setup.mode.into())]);
        if let Some(error) = failure {
            log.warn("log file unavailable", &[("err", Field::error(&error))]);
        }
        let reports = reports(setup.env, &version, &log);
        Self {
            version,
            // The core sends nothing until it can ask the person and store their answer.
            reporter: Arc::new(weacon_appkit::reporter(reports, false, setup.deliver)),
            log,
            state_dir,
            log_file,
            spawn: platform::spawn_policy(setup.env),
            path: (setup.env)("PATH"),
        }
    }

    /// Logs and reports how the command ended, and gives its exit code.
    pub(crate) fn finish(&self, result: Result<Outcome, Fault>) -> ExitCode {
        weacon_appkit::finish(&self.log, &self.reporter, result)
    }

    /// Logs and reports a panic, without its message or location.
    pub(crate) fn panic_hook(&self) -> Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync> {
        panic_hook(self.log.clone(), Arc::clone(&self.reporter), panicked)
    }
}

fn panicked() -> Fault {
    Fault::new(code!("core.panicked"), "The core panicked")
}

pub(crate) fn state_unavailable() -> Fault {
    Fault::new(
        code!("state.unavailable"),
        "The state folder can't be used: set HOME or WEACON_STATE_DIR to a writable folder",
    )
}

fn open_log(state_dir: &Path, name: &str) -> io::Result<(PathBuf, RotatingFile)> {
    let dir = state_dir.join("logs");
    platform::private_dir(state_dir)?;
    platform::private_dir(&dir)?;
    let path = dir.join(name);
    let file = RotatingFile::open(path.clone(), MAX_BYTES, KEEP)?;
    Ok((path, file))
}

fn nothing() -> Sink {
    Box::new(|_| {})
}

fn reports(env: Env<'_>, version: &Version, log: &Logger) -> Option<Reports> {
    let dsn = platform::var(env, "WEACON_SENTRY_DSN")?;
    let home_dir = platform::home_dir(env).unwrap_or_default();
    weacon_appkit::reports(
        &dsn.to_string_lossy(),
        &format!("weacon/{}", version.version),
        version,
        &home_dir.to_string_lossy(),
        log,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::code;
    use weacon_testing::{TempDir, Uploads, recording_deliver};

    use super::*;
    use crate::test_support::{Records, VERSION, env, messages, recording_sink};

    const DSN: &str = "https://key@sentry.example.com/1";

    fn start(vars: &[(&str, &str)], verbose: bool) -> (App, Records, Uploads) {
        let env = env(vars);
        let (stderr, records) = recording_sink();
        let (deliver, uploads) = recording_deliver();
        let app = App::start(
            VERSION,
            Setup {
                env: &env,
                stderr,
                deliver,
                verbose,
                mode: "doctor",
                log_name: "cli.log",
            },
        );
        (app, records, uploads)
    }

    fn state(dir: &Path) -> (&'static str, &str) {
        ("WEACON_STATE_DIR", dir.to_str().unwrap())
    }

    #[test]
    fn logs_go_to_the_state_folder_starting_with_the_build() {
        let dir = TempDir::new().unwrap();
        let (app, stderr, _) = start(&[state(&dir)], false);
        let file = app.log_file.clone().unwrap();
        let text = fs::read_to_string(&file).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();

        assert_eq!(file, dir.join("logs/cli.log"));
        assert_eq!(first["msg"], "app started");
        assert_eq!(first["commit"], "1a2b3c4");
        assert_eq!(first["mode"], "doctor");
        assert!(messages(&stderr).is_empty());
        assert!(!app.reporter.is_on());
    }

    #[test]
    fn verbose_also_prints_info_logs() {
        let dir = TempDir::new().unwrap();
        let (_, stderr, _) = start(&[state(&dir)], true);

        assert_eq!(messages(&stderr), ["app started"]);
    }

    #[test]
    fn without_a_usable_state_folder_logs_still_reach_stderr() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("a-file");
        fs::write(&file, "").unwrap();
        let (unknown, unknown_stderr, _) = start(&[], false);
        let (blocked, blocked_stderr, _) = start(&[state(&file)], false);

        assert_eq!(unknown.state_dir, None);
        assert!(messages(&unknown_stderr).is_empty());
        assert_eq!(blocked.log_file, None);
        assert_eq!(messages(&blocked_stderr), ["log file unavailable"]);
    }

    #[test]
    fn a_bad_dsn_turns_reports_off_with_a_warning() {
        let (app, stderr, _) = start(&[("WEACON_SENTRY_DSN", "not a dsn")], false);

        assert!(!app.reporter.is_on());
        assert_eq!(messages(&stderr), ["report setup failed"]);
    }

    #[test]
    fn a_dsn_alone_sends_nothing() {
        let (app, _, uploads) = start(&[("WEACON_SENTRY_DSN", DSN)], false);
        let fault = Fault::new(code!("store.full"), "The store is full");

        assert!(app.reporter.is_on());
        assert!(!app.reporter.agreed());
        assert_eq!(app.finish(Err(fault)), ExitCode::from(2));
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn success_exits_cleanly_reports_nothing_and_says_nothing_on_stderr() {
        let (app, stderr, uploads) = start(&[("WEACON_SENTRY_DSN", DSN)], false);
        app.reporter.set_agreed(true);

        assert!(app.reporter.is_on());
        assert_eq!(app.finish(Ok(().into())), ExitCode::SUCCESS);
        assert!(uploads.lock().unwrap().is_empty());
        assert!(messages(&stderr).is_empty());
    }

    #[test]
    fn a_panic_is_reported_as_the_cores() {
        assert_eq!(panicked().code().as_str(), "core.panicked");
    }

    #[test]
    fn no_content_reaches_a_log_line_or_an_error_report() {
        let dir = TempDir::new().unwrap();
        let vars = [
            state(&dir),
            ("WEACON_SENTRY_DSN", DSN),
            ("HOME", "/home/ada"),
        ];
        let (app, stderr, uploads) = start(&vars, false);
        // Agreed, so the markers go through a real report.
        app.reporter.set_agreed(true);
        let leak = io::Error::other(
            "could not open /home/ada/MARKER-PATH: token MARKER-TOKEN, MARKER-MESSAGE",
        );
        let fault = Fault::new(code!("store.full"), "The store is full").because(leak);
        app.log.info(
            "run started",
            &[
                ("token", "MARKER-TOKEN".into()),
                ("prompt", "MARKER-MESSAGE".into()),
            ],
        );
        let exit = app.finish(Err(fault));
        let file = fs::read_to_string(app.log_file.as_ref().unwrap()).unwrap();
        let stderr = serde_json::to_string(&*stderr.lock().unwrap()).unwrap();
        let uploads = uploads.lock().unwrap();

        assert_eq!(exit, ExitCode::from(2));
        assert_eq!(uploads.len(), 1);
        assert!(file.contains("app failed"), "{file}");
        for text in [
            file,
            stderr,
            String::from_utf8_lossy(&uploads[0].body).into_owned(),
        ] {
            assert!(!text.contains("MARKER"), "{text}");
        }
    }
}
