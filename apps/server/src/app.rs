use std::sync::Arc;

use weacon_appkit::{App, random_id, reporter, reports, root_logger};
use weacon_observability::{Deliver, Fault, Sink};
use weacon_protocol::code;
use weacon_version::Version;

/// Starts the server's app: it logs to `stdout`, and sends error reports when `dsn` is set.
pub(crate) fn start(version: Version, stdout: Sink, deliver: Deliver, dsn: Option<&str>) -> App {
    let log = root_logger(stdout, &version);
    log.info("app started", &[]);
    let client = format!("{}/{}", version.name, version.version);
    let reports = dsn.and_then(|dsn| reports(dsn, &client, &version, "", &log));
    App {
        version,
        reporter: Arc::new(reporter(reports, true, deliver)),
        log,
    }
}

/// A new random id, for a request or a row.
pub(crate) fn new_id() -> String {
    format!("{:032x}", random_id())
}

/// A new id from `new_id`, checked as a `T`.
pub(crate) fn fresh<T: TryFrom<String>>(new_id: &dyn Fn() -> String) -> Result<T, Fault> {
    T::try_from(new_id()).map_err(|_| Fault::new(code!("id.invalid"), "A new id isn't valid"))
}

pub(crate) fn panicked() -> Fault {
    Fault::new(code!("server.panicked"), "The server panicked")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::process::ExitCode;
    use std::sync::Mutex;

    use weacon_observability::Upload;

    use super::*;
    use crate::test_support::{Records, VERSION, messages, recording_sink};

    const DSN: &str = "https://key@sentry.example.com/1";

    fn started(dsn: Option<&str>) -> (App, Records, Arc<Mutex<Vec<Upload>>>) {
        let (stdout, records) = recording_sink();
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&uploads);
        let deliver: Deliver = Box::new(move |upload| {
            seen.lock().unwrap().push(upload.clone());
            Ok(())
        });
        (start(VERSION, stdout, deliver, dsn), records, uploads)
    }

    #[test]
    fn logs_start_with_the_build() {
        let (_, records, _) = started(None);
        let first = records.lock().unwrap()[0].clone();

        assert_eq!(first.msg, "app started");
        assert_eq!(first.fields["name"], "weacon-server");
        assert_eq!(first.fields["version"], "0.1.0");
        assert_eq!(first.fields["commit"], "1a2b3c4");
    }

    #[test]
    fn a_bad_dsn_turns_reports_off_with_a_warning() {
        let (app, records, uploads) = started(Some("not a dsn"));
        let exit = app.finish(Err(Fault::new(code!("store.full"), "The store is full")));

        assert_eq!(exit, ExitCode::from(2));

        assert_eq!(
            messages(&records),
            ["app started", "report setup failed", "app failed"]
        );
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn success_exits_cleanly_and_reports_nothing() {
        let (app, records, uploads) = started(Some(DSN));

        assert_eq!(app.finish(Ok(().into())), ExitCode::SUCCESS);
        assert_eq!(messages(&records), ["app started", "app stopped"]);
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn a_panic_is_reported_as_the_servers() {
        assert_eq!(panicked().code().as_str(), "server.panicked");
    }

    #[test]
    fn no_content_reaches_a_log_line_or_an_error_report() {
        let (app, records, uploads) = started(Some(DSN));
        let leak =
            io::Error::other("could not open /srv/MARKER-PATH: token MARKER-TOKEN, MARKER-MESSAGE");
        let fault = Fault::new(code!("store.full"), "The store is full").because(leak);
        app.log.info(
            "sign-in started",
            &[
                ("token", "MARKER-TOKEN".into()),
                ("message", "MARKER-MESSAGE".into()),
            ],
        );
        let exit = app.finish(Err(fault));
        let logs = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        let uploads = uploads.lock().unwrap();

        assert_eq!(exit, ExitCode::from(2));
        assert_eq!(uploads.len(), 1);
        assert!(logs.contains("app failed"), "{logs}");
        for text in [logs, String::from_utf8_lossy(&uploads[0].body).into_owned()] {
            assert!(!text.contains("MARKER"), "{text}");
        }
    }
}
