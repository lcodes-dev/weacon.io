use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use weacon_observability::{Deliver, Level, Logger, LoggerOptions, Record, Sink, Upload};

/// The records a recording sink has seen.
pub type Records = Arc<Mutex<Vec<Record>>>;

/// The error reports a recording `Deliver` has been given.
pub type Uploads = Arc<Mutex<Vec<Upload>>>;

/// A sink that keeps every record it is given.
#[must_use]
pub fn recording_sink() -> (Sink, Records) {
    let records = Records::default();
    let seen = Arc::clone(&records);
    let sink: Sink = Box::new(move |record| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record.clone());
    });
    (sink, records)
}

/// A `Deliver` that keeps every report instead of sending it.
#[must_use]
pub fn recording_deliver() -> (Deliver, Uploads) {
    let uploads = Uploads::default();
    let seen = Arc::clone(&uploads);
    let deliver: Deliver = Box::new(move |upload| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(upload.clone());
        Ok(())
    });
    (deliver, uploads)
}

/// An info-level logger that keeps every record.
#[must_use]
pub fn recording_logger() -> (Logger, Records) {
    let (sink, records) = recording_sink();
    let log = Logger::new(LoggerOptions {
        sink,
        now: Box::new(SystemTime::now),
        level: Level::Info,
    });
    (log, records)
}

/// The messages of the records seen so far.
#[must_use]
pub fn messages(records: &Records) -> Vec<&'static str> {
    records
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|record| record.msg)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_each_record_in_order() {
        let (log, records) = recording_logger();
        log.debug("ignored", &[]);
        log.info("run started", &[]);
        log.warn("sync failed", &[]);

        assert_eq!(messages(&records), ["run started", "sync failed"]);
    }

    #[test]
    fn keeps_each_report_in_order() {
        let (deliver, uploads) = recording_deliver();
        let upload = |body: &str| Upload {
            url: "https://sentry.example.com/api/1/envelope/".to_owned(),
            auth: "Sentry sentry_key=key".to_owned(),
            body: body.as_bytes().to_vec(),
        };

        deliver(&upload("one")).unwrap();
        deliver(&upload("two")).unwrap();

        assert_eq!(*uploads.lock().unwrap(), [upload("one"), upload("two")]);
    }
}
