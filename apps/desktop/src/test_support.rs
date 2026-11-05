use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use weacon_observability::{Level, Logger, LoggerOptions, Record, Sink};
use weacon_protocol::{Envelope, Message, RunUpdate};
use weacon_testing::fake_core::{SESSION, recording};
use weacon_version::Version;

pub(crate) const VERSION: Version = Version {
    name: "weacon-desktop",
    version: "0.1.0",
    commit: "1a2b3c4",
};

pub(crate) type Records = Arc<Mutex<Vec<Record>>>;

pub(crate) fn recording_sink() -> (Sink, Records) {
    let records = Records::default();
    let seen = Arc::clone(&records);
    let sink: Sink = Box::new(move |record| seen.lock().unwrap().push(record.clone()));
    (sink, records)
}

pub(crate) fn recording_logger() -> (Logger, Records) {
    let (sink, records) = recording_sink();
    let log = Logger::new(LoggerOptions {
        sink,
        now: Box::new(SystemTime::now),
        level: Level::Info,
    });
    (log, records)
}

pub(crate) fn messages(records: &Records) -> Vec<&'static str> {
    records
        .lock()
        .unwrap()
        .iter()
        .map(|record| record.msg)
        .collect()
}

/// The run updates a core sends for the recorded session, parsed as the desktop parses them.
pub(crate) fn updates(tokens: usize) -> Vec<RunUpdate> {
    recording(SESSION, tokens)
        .unwrap()
        .lines()
        .map(|line| {
            let json = line.strip_prefix("< ").unwrap().as_bytes();
            let Message::Run(update) = Envelope::from_json(json).unwrap().message else {
                panic!("not a run update: {line}");
            };
            update
        })
        .collect()
}
