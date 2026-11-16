//! Hands each report to the sender, when reports are on.

use std::error::Error;

use weacon_protocol::Code;

use crate::{Clock, Fault, Field, Logger, Reports, Upload};

/// Delivers one report; the real one POSTs it, tests record it.
pub type Deliver = Box<dyn Fn(&Upload) -> Result<(), Fault> + Send + Sync>;

/// Sends error reports when whoever runs the program set an endpoint, and does nothing
/// otherwise.
pub struct Reporter {
    reports: Option<Reports>,
    deliver: Deliver,
    now: Clock,
}

impl Reporter {
    #[must_use]
    pub fn new(reports: Option<Reports>, deliver: Deliver, now: Clock) -> Self {
        Self {
            reports,
            deliver,
            now,
        }
    }

    /// Whether an endpoint is set.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.reports.is_some()
    }

    /// Reports `error` under `code`; a report that can't be sent is logged and dropped.
    pub fn capture_error<E: Error + 'static>(&self, error: &E, code: &Code, log: &Logger) {
        let Some(reports) = &self.reports else {
            return;
        };
        let upload = reports.upload(error, code, (self.now)());
        if let Err(error) = (self.deliver)(&upload) {
            log.warn("report failed", &[("err", Field::error(&error))]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use weacon_protocol::{App, code};

    use super::*;
    use crate::Level;
    use crate::logger::tests::capture;

    fn recorder(fails: bool) -> (Deliver, Arc<Mutex<Vec<Upload>>>) {
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&uploads);
        let deliver: Deliver = Box::new(move |upload| {
            seen.lock().unwrap().push(upload.clone());
            if fails {
                return Err(Fault::new(code!("report.send_failed"), "failed"));
            }
            Ok(())
        });
        (deliver, uploads)
    }

    fn reporter(on: bool, deliver: Deliver) -> Reporter {
        let build = App {
            name: "weacon-core".to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        };
        Reporter::new(
            on.then(|| Reports::new("http://localhost:4318", build)),
            deliver,
            Box::new(|| UNIX_EPOCH + Duration::from_hours(489_466)),
        )
    }

    fn fault() -> Fault {
        Fault::new(code!("store.full"), "The store is full")
    }

    #[test]
    fn without_an_endpoint_nothing_is_sent() {
        let (deliver, uploads) = recorder(false);
        let reporter = reporter(false, deliver);
        reporter.capture_error(&fault(), &code!("store.full"), &capture(Level::Info).0);

        assert!(!reporter.is_on());
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn with_an_endpoint_each_error_is_sent_with_the_time() {
        let (deliver, uploads) = recorder(false);
        let reporter = reporter(true, deliver);
        let (log, records) = capture(Level::Info);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        let uploads = uploads.lock().unwrap();
        let first = String::from_utf8_lossy(&uploads[0].body).into_owned();

        assert!(reporter.is_on());
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[0].url, "http://localhost:4318/v1/logs");
        assert!(
            first.contains(r#""timeUnixNano":"1762077600000000000""#),
            "{first}"
        );
        assert!(records.lock().unwrap().is_empty());
    }

    #[test]
    fn a_report_that_cant_be_sent_is_logged() {
        let (deliver, _) = recorder(true);
        let reporter = reporter(true, deliver);
        let (log, records) = capture(Level::Info);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        let records = records.lock().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].msg, "report failed");
        assert_eq!(records[0].level, Level::Warn);
    }
}
