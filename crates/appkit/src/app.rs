use std::panic::PanicHookInfo;
use std::sync::Arc;
use std::time::SystemTime;

use weacon_observability::{
    Deliver, Fault, Field, Level, Logger, LoggerOptions, Reporter, Reports, Sink,
};
use weacon_protocol::App;
use weacon_version::Version;

use crate::random_id;

/// How a build introduces itself to other programs.
#[must_use]
pub fn build(version: &Version) -> App {
    App {
        name: version.name.to_owned(),
        version: version.version.to_owned(),
        commit: version.commit.to_owned(),
    }
}

/// An app's root logger: info and up, with its build on every record.
#[must_use]
pub fn root_logger(sink: Sink, version: &Version) -> Logger {
    Logger::new(LoggerOptions {
        sink,
        now: Box::new(SystemTime::now),
        level: Level::Info,
    })
    .child(&[
        ("name", version.name.into()),
        ("version", version.version.into()),
        ("commit", version.commit.into()),
    ])
}

/// Reports for `dsn`, from `client` running `version`; a bad DSN is logged and turns them off.
#[must_use]
pub fn reports(
    dsn: &str,
    client: &str,
    version: &Version,
    home_dir: &str,
    log: &Logger,
) -> Option<Reports> {
    Reports::new(dsn, client, &version.release(), home_dir)
        .inspect_err(|fault| log.warn("report setup failed", &[("err", Field::error(fault))]))
        .ok()
}

/// A reporter with random ids and the system clock; see [`Reporter::new`] for `agreed`.
#[must_use]
pub fn reporter(reports: Option<Reports>, agreed: bool, deliver: Deliver) -> Reporter {
    Reporter::new(
        reports,
        agreed,
        deliver,
        Box::new(random_id),
        Box::new(SystemTime::now),
    )
}

/// Logs and reports a panic as `fault`, without its message or location.
#[must_use]
pub fn panic_hook(
    log: Logger,
    reporter: Arc<Reporter>,
    fault: fn() -> Fault,
) -> Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync> {
    Box::new(move |_| panicked(&log, &reporter, &fault()))
}

fn panicked(log: &Logger, reporter: &Reporter, fault: &Fault) {
    // An app survives a panic in one task, so this is an error, not fatal.
    log.error("panicked", &[("err", Field::error(fault))]);
    reporter.capture_error(fault, fault.code(), log);
}

#[cfg(test)]
mod tests {

    use weacon_protocol::code;
    use weacon_testing::{messages, recording_sink};

    use super::*;
    use crate::test_support::recording_reporter;

    const VERSION: Version = Version {
        name: "weacon-core",
        version: "0.1.0",
        commit: "1a2b3c4",
    };
    const DSN: &str = "https://key@sentry.example.com/1";

    #[test]
    fn a_build_introduces_itself_by_name_version_and_commit() {
        let app = build(&VERSION);

        assert_eq!(
            (app.name.as_str(), app.version.as_str(), app.commit.as_str()),
            ("weacon-core", "0.1.0", "1a2b3c4")
        );
    }

    #[test]
    fn every_record_carries_the_build() {
        let (sink, records) = recording_sink();
        let log = root_logger(sink, &VERSION);
        log.debug("ignored", &[]);
        log.info("app started", &[]);
        let record = records.lock().unwrap()[0].clone();

        assert_eq!(messages(&records), ["app started"]);
        assert_eq!(record.fields["name"], "weacon-core");
        assert_eq!(record.fields["version"], "0.1.0");
        assert_eq!(record.fields["commit"], "1a2b3c4");
    }

    #[test]
    fn a_bad_dsn_turns_reports_off_with_a_warning() {
        let (sink, records) = recording_sink();
        let log = root_logger(sink, &VERSION);

        assert!(reports("not a dsn", "weacon/0.1.0", &VERSION, "", &log).is_none());
        assert!(reports(DSN, "weacon/0.1.0", &VERSION, "", &log).is_some());
        assert_eq!(messages(&records), ["report setup failed"]);
    }

    #[test]
    fn a_panic_is_logged_and_reported_without_its_text() {
        let (sink, records) = recording_sink();
        let log = root_logger(sink, &VERSION);
        let (reporter, uploads) =
            recording_reporter(reports(DSN, "weacon/0.1.0", &VERSION, "", &log));
        panicked(
            &log,
            &reporter,
            &Fault::new(code!("core.panicked"), "The core panicked"),
        );
        let record = records.lock().unwrap().pop().unwrap();
        let uploads = uploads.lock().unwrap();
        let body = String::from_utf8_lossy(&uploads[0].body);

        assert_eq!(record.msg, "panicked");
        assert_eq!(record.level, Level::Error);
        assert_eq!(uploads.len(), 1);
        assert!(body.contains("core.panicked"), "{body}");
        assert!(body.contains("weacon-core@0.1.0+1a2b3c4"), "{body}");
    }
}
