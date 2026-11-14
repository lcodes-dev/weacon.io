//! Decides whether each report goes out, and hands it to the sender.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};

use weacon_protocol::Code;

use crate::{Clock, Fault, Field, Logger, Reports, Upload};

/// Delivers one report; the real one POSTs it, tests record it.
pub type Deliver = Box<dyn Fn(&Upload) -> Result<(), Fault> + Send + Sync>;

/// Gives each report a new random id.
pub type Ids = Box<dyn Fn() -> u128 + Send + Sync>;

/// Sends error reports when a DSN is set and the person has agreed, and does nothing otherwise.
pub struct Reporter {
    reports: Option<Reports>,
    agreed: AtomicBool,
    deliver: Deliver,
    ids: Ids,
    now: Clock,
}

impl Reporter {
    /// `agreed` is whether the person has agreed to send reports; where there is no one to
    /// ask, as on a team's own server, it is `true`.
    #[must_use]
    pub fn new(
        reports: Option<Reports>,
        agreed: bool,
        deliver: Deliver,
        ids: Ids,
        now: Clock,
    ) -> Self {
        Self {
            reports,
            agreed: AtomicBool::new(agreed),
            deliver,
            ids,
            now,
        }
    }

    /// Whether reports can be sent at all: a DSN is set.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.reports.is_some()
    }

    /// Records the person's answer, which the next error goes by.
    pub fn set_agreed(&self, agreed: bool) {
        self.agreed.store(agreed, Ordering::Relaxed);
    }

    #[must_use]
    pub fn agreed(&self) -> bool {
        self.agreed.load(Ordering::Relaxed)
    }

    /// Reports `error` under `code`; a report that can't be sent is logged and dropped.
    pub fn capture_error<E: Error + 'static>(&self, error: &E, code: &Code, log: &Logger) {
        let Some(reports) = self.reports.as_ref().filter(|_| self.agreed()) else {
            return;
        };
        let upload = reports.upload(error, code, (self.ids)(), (self.now)());
        if let Err(error) = (self.deliver)(&upload) {
            log.warn("report failed", &[("err", Field::error(&error))]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use weacon_protocol::code;

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

    fn reporter(reports: Option<Reports>, deliver: Deliver) -> Reporter {
        let next = AtomicU64::new(1);
        Reporter::new(
            reports,
            true,
            deliver,
            Box::new(move || u128::from(next.fetch_add(1, Ordering::Relaxed))),
            Box::new(|| UNIX_EPOCH + Duration::from_hours(489_466)),
        )
    }

    fn reports() -> Option<Reports> {
        Reports::new(
            "https://public@sentry.example.com/42",
            "weacon/0.1.0",
            "weacon-core@0.1.0+1a2b3c4",
            "/home/ada",
        )
        .ok()
    }

    fn fault() -> Fault {
        Fault::new(code!("store.full"), "The store is full")
    }

    #[test]
    fn without_a_dsn_nothing_is_sent() {
        let (deliver, uploads) = recorder(false);
        let reporter = reporter(None, deliver);
        reporter.capture_error(&fault(), &code!("store.full"), &capture(Level::Info).0);

        assert!(!reporter.is_on());
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn with_a_dsn_each_error_is_sent_once_with_its_own_id_and_time() {
        let (deliver, uploads) = recorder(false);
        let reporter = reporter(reports(), deliver);
        let (log, records) = capture(Level::Info);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        let uploads = uploads.lock().unwrap();
        let first = String::from_utf8_lossy(&uploads[0].body).into_owned();

        assert!(reporter.is_on());
        assert_eq!(uploads.len(), 2);
        assert_ne!(uploads[0].body, uploads[1].body);
        assert!(
            first.contains("00000000000000000000000000000001"),
            "{first}"
        );
        assert!(first.contains("\"timestamp\":1762077600"), "{first}");
        assert!(first.contains("weacon-core@0.1.0+1a2b3c4"), "{first}");
        assert!(records.lock().unwrap().is_empty());
    }

    #[test]
    fn a_report_that_cant_be_sent_is_logged() {
        let (deliver, _) = recorder(true);
        let reporter = reporter(reports(), deliver);
        let (log, records) = capture(Level::Info);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        let records = records.lock().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].msg, "report failed");
        assert_eq!(records[0].level, Level::Warn);
    }

    #[test]
    fn nothing_is_sent_until_the_person_agrees() {
        let (deliver, uploads) = recorder(false);
        let reporter = reporter(reports(), deliver);
        let log = capture(Level::Info).0;
        reporter.set_agreed(false);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        let before = uploads.lock().unwrap().len();
        reporter.set_agreed(true);
        reporter.capture_error(&fault(), &code!("store.full"), &log);

        assert_eq!(before, 0);
        assert!(reporter.agreed());
        assert_eq!(uploads.lock().unwrap().len(), 1);
    }
}
