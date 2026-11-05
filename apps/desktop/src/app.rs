use std::io;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use weacon_client::platform::{Env, home_dir, state_dir, var};
use weacon_observability::{Fault, Field, Level, Logger, LoggerOptions, Reports, Sink, json_lines};
use weacon_protocol::code;
use weacon_version::Version;

use crate::platform;
use crate::report::{Deliver, Reporter};

/// The desktop's build, logger, error reporter and where the core's state lives.
pub(crate) struct App {
    pub(crate) version: Version,
    pub(crate) log: Logger,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) state_dir: Option<PathBuf>,
}

/// The outside world an [`App`] starts from; `main` passes the real one.
pub(crate) struct Setup<'a> {
    pub(crate) env: Env<'a>,
    pub(crate) stderr: Sink,
    pub(crate) deliver: Deliver,
}

impl App {
    pub(crate) fn start(version: Version, setup: Setup<'_>) -> Self {
        let state_dir = state_dir(setup.env);
        let (file_sink, failure) = match state_dir.as_deref().map(open_log) {
            Some(Ok(sink)) => (sink, None),
            Some(Err(error)) => (nothing(), Some(error)),
            None => (nothing(), None),
        };
        let stderr = setup.stderr;
        let log = Logger::new(LoggerOptions {
            sink: Box::new(move |record| {
                file_sink(record);
                if record.level >= Level::Warn {
                    stderr(record);
                }
            }),
            now: Box::new(SystemTime::now),
            level: Level::Info,
        })
        .child(&[
            ("name", version.name.into()),
            ("version", version.version.into()),
            ("commit", version.commit.into()),
        ]);
        log.info("app started", &[]);
        if let Some(error) = failure {
            log.warn("log file unavailable", &[("err", Field::error(&error))]);
        }
        let reports = reports(setup.env, &version, &log);
        Self {
            version,
            reporter: Arc::new(Reporter::new(reports, setup.deliver)),
            log,
            state_dir,
        }
    }

    /// Logs and reports a fault the person has to look at.
    pub(crate) fn fail(&self, fault: &Fault) {
        self.log
            .error("desktop failed", &[("err", Field::error(fault))]);
        self.reporter.capture_error(fault, fault.code(), &self.log);
    }

    /// Logs and reports a panic, without its message or location.
    pub(crate) fn panic_hook(&self) -> Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync> {
        let (log, reporter) = (self.log.clone(), Arc::clone(&self.reporter));
        Box::new(move |_| panicked(&log, &reporter))
    }
}

pub(crate) fn state_unavailable() -> Fault {
    Fault::new(
        code!("state.unavailable"),
        "The state folder can't be found: set HOME or WEACON_STATE_DIR",
    )
}

pub(crate) fn window_failed(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Fault {
    Fault::new(code!("desktop.window_failed"), "The window couldn't open").because(error)
}

fn panicked(log: &Logger, reporter: &Reporter) {
    let fault = Fault::new(code!("desktop.panicked"), "The desktop panicked");
    log.fatal("panicked", &[("err", Field::error(&fault))]);
    reporter.capture_error(&fault, fault.code(), log);
}

fn open_log(state_dir: &Path) -> io::Result<Sink> {
    let dir = state_dir.join("logs");
    platform::private_dir(&dir)?;
    Ok(json_lines(platform::private_append(
        &dir.join("desktop.log"),
    )?))
}

fn nothing() -> Sink {
    Box::new(|_| {})
}

fn reports(env: Env<'_>, version: &Version, log: &Logger) -> Option<Reports> {
    let dsn = var(env, "WEACON_SENTRY_DSN")?;
    let home_dir = home_dir(env).unwrap_or_default();
    Reports::new(
        &dsn.to_string_lossy(),
        &format!("weacon-desktop/{}", version.version),
        &version.release(),
        &home_dir.to_string_lossy(),
    )
    .inspect_err(|fault| log.warn("report setup failed", &[("err", Field::error(fault))]))
    .ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::Mutex;

    use weacon_observability::Upload;

    use super::*;
    use crate::test_support::{Records, VERSION, messages, recording_sink};

    const DSN: &str = "https://key@sentry.example.com/1";

    type Uploads = Arc<Mutex<Vec<Upload>>>;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("weacon-desktop-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn start(vars: &[(&str, &str)]) -> (App, Records, Uploads) {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        let env = move |key: &str| vars.get(key).cloned();
        let (stderr, records) = recording_sink();
        let uploads = Uploads::default();
        let seen = Arc::clone(&uploads);
        let app = App::start(
            VERSION,
            Setup {
                env: &env,
                stderr,
                deliver: Box::new(move |upload| {
                    seen.lock().unwrap().push(upload.clone());
                    Ok(())
                }),
            },
        );
        (app, records, uploads)
    }

    fn log_lines(dir: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(dir.join("logs/desktop.log"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn logs_go_to_the_state_folder_starting_with_the_build() {
        let dir = temp_dir("logs");
        let (app, stderr, _) = start(&[("WEACON_STATE_DIR", dir.to_str().unwrap())]);
        let lines = log_lines(&dir);

        assert_eq!(app.state_dir.as_deref(), Some(dir.as_path()));
        assert_eq!(lines[0]["msg"], "app started");
        assert_eq!(lines[0]["name"], "weacon-desktop");
        assert_eq!(lines[0]["commit"], "1a2b3c4");
        assert!(messages(&stderr).is_empty());
        assert!(!app.reporter.is_available());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_usable_state_folder_warnings_still_reach_stderr() {
        let dir = temp_dir("blocked");
        let file = dir.join("a-file");
        fs::write(&file, "").unwrap();
        let (unknown, unknown_stderr, _) = start(&[]);
        let (_, blocked_stderr, _) = start(&[("WEACON_STATE_DIR", file.to_str().unwrap())]);

        assert_eq!(unknown.state_dir, None);
        assert!(messages(&unknown_stderr).is_empty());
        assert_eq!(messages(&blocked_stderr), ["log file unavailable"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_are_reported_only_once_the_person_agrees() {
        let (app, stderr, uploads) = start(&[("WEACON_SENTRY_DSN", DSN)]);
        let fault = Fault::new(code!("store.full"), "The store is full");
        app.fail(&fault);
        let before = uploads.lock().unwrap().len();
        app.reporter.set_agreed(true);
        app.fail(&fault);
        panicked(&app.log, &app.reporter);
        let uploads = uploads.lock().unwrap();

        assert!(app.reporter.is_available());
        assert_eq!(before, 0);
        assert_eq!(uploads.len(), 2);
        assert!(String::from_utf8_lossy(&uploads[1].body).contains("desktop.panicked"));
        assert_eq!(
            messages(&stderr),
            ["desktop failed", "desktop failed", "panicked"]
        );
    }

    #[test]
    fn a_bad_dsn_turns_reports_off_with_a_warning() {
        let (app, stderr, _) = start(&[("WEACON_SENTRY_DSN", "not a dsn")]);

        assert!(!app.reporter.is_available());
        assert_eq!(messages(&stderr), ["report setup failed"]);
    }

    #[test]
    fn no_content_reaches_a_log_line_or_an_error_report() {
        let dir = temp_dir("markers");
        let vars = [
            ("WEACON_STATE_DIR", dir.to_str().unwrap()),
            ("WEACON_SENTRY_DSN", DSN),
            ("HOME", "/home/ada"),
        ];
        let (app, stderr, uploads) = start(&vars);
        app.reporter.set_agreed(true);
        let leak = io::Error::other(
            "could not open /home/ada/MARKER-PATH: token MARKER-TOKEN, MARKER-MESSAGE",
        );
        app.log.info(
            "run started",
            &[
                ("token", "MARKER-TOKEN".into()),
                ("prompt", "MARKER-MESSAGE".into()),
            ],
        );
        app.fail(&Fault::new(code!("store.full"), "The store is full").because(leak));
        let file = fs::read_to_string(dir.join("logs/desktop.log")).unwrap();
        let stderr = serde_json::to_string(&*stderr.lock().unwrap()).unwrap();
        let uploads = uploads.lock().unwrap();

        assert_eq!(uploads.len(), 1);
        assert!(file.contains("desktop failed"), "{file}");
        for text in [
            file,
            stderr,
            String::from_utf8_lossy(&uploads[0].body).into_owned(),
        ] {
            assert!(!text.contains("MARKER"), "{text}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
