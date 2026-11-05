use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use weacon_observability::{Level, Logger, LoggerOptions, Record, Sink};
use weacon_version::Version;

pub(crate) const VERSION: Version = Version {
    name: "weacon-core",
    version: "0.1.0",
    commit: "1a2b3c4",
};

pub(crate) type Records = Arc<Mutex<Vec<Record>>>;

/// A fake environment holding only `vars`.
pub(crate) fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let vars: HashMap<String, OsString> = vars
        .iter()
        .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
        .collect();
    move |key| vars.get(key).cloned()
}

/// An empty folder of this test's own, readable only by this user.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("weacon-core-{name}-{}", std::process::id()));
    drop(fs::remove_dir_all(&dir));
    crate::platform::private_dir(&dir).unwrap();
    dir
}

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

/// The code of an error message, if it is one.
pub(crate) fn error_code(message: &weacon_protocol::Message) -> Option<&str> {
    match message {
        weacon_protocol::Message::Error(body) => Some(body.code.as_str()),
        weacon_protocol::Message::Hello(_)
        | weacon_protocol::Message::Welcome(_)
        | weacon_protocol::Message::Receipt(_)
        | weacon_protocol::Message::Run(_)
        | weacon_protocol::Message::Unknown => None,
    }
}
