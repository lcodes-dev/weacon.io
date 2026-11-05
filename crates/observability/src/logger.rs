use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::redact::{Field, redact_fields};

const RESERVED: [&str; 3] = ["time", "level", "msg"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
    Fatal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Record {
    #[serde(flatten)]
    pub fields: Map<String, Value>,
    /// Milliseconds since the Unix epoch; 0 when the clock is set before it.
    pub time: u64,
    pub level: Level,
    pub msg: &'static str,
}

pub type Sink = Box<dyn Fn(&Record) + Send + Sync>;
pub type Clock = Box<dyn Fn() -> SystemTime + Send + Sync>;

pub struct LoggerOptions {
    pub sink: Sink,
    pub now: Clock,
    pub level: Level,
}

/// Writes records at or above its level, with redacted fields. Messages are fixed strings:
///
/// ```compile_fail
/// fn said(log: &weacon_observability::Logger, text: String) {
///     log.info(&text, &[]);
/// }
/// ```
#[derive(Clone)]
pub struct Logger {
    options: Arc<LoggerOptions>,
    bindings: Arc<Map<String, Value>>,
}

impl Logger {
    #[must_use]
    pub fn new(options: LoggerOptions) -> Self {
        Self {
            options: Arc::new(options),
            bindings: Arc::default(),
        }
    }

    /// A logger that adds `fields` to every record.
    #[must_use]
    pub fn child(&self, fields: &[(&str, Field)]) -> Self {
        let mut bindings = (*self.bindings).clone();
        bindings.extend(redact_fields(fields));
        Self {
            options: Arc::clone(&self.options),
            bindings: Arc::new(bindings),
        }
    }

    pub fn trace(&self, msg: &'static str, fields: &[(&str, Field)]) {
        self.write(Level::Trace, msg, fields);
    }

    pub fn debug(&self, msg: &'static str, fields: &[(&str, Field)]) {
        self.write(Level::Debug, msg, fields);
    }

    pub fn info(&self, msg: &'static str, fields: &[(&str, Field)]) {
        self.write(Level::Info, msg, fields);
    }

    pub fn warn(&self, msg: &'static str, fields: &[(&str, Field)]) {
        self.write(Level::Warn, msg, fields);
    }

    pub fn error(&self, msg: &'static str, fields: &[(&str, Field)]) {
        self.write(Level::Error, msg, fields);
    }

    pub fn fatal(&self, msg: &'static str, fields: &[(&str, Field)]) {
        self.write(Level::Fatal, msg, fields);
    }

    fn write(&self, level: Level, msg: &'static str, fields: &[(&str, Field)]) {
        if level < self.options.level {
            return;
        }
        let mut all = (*self.bindings).clone();
        all.extend(redact_fields(fields));
        for key in RESERVED {
            all.remove(key);
        }
        let time = (self.options.now)()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        (self.options.sink)(&Record {
            fields: all,
            time,
            level,
            msg,
        });
    }
}

impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Logger")
            .field("level", &self.options.level)
            .field("bindings", &self.bindings.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Writes each record as one line of JSON.
pub fn json_lines(writer: impl Write + Send + 'static) -> Sink {
    let writer = Mutex::new(writer);
    Box::new(move |record: &Record| {
        let Ok(mut line) = serde_json::to_vec(record) else {
            return;
        };
        line.push(b'\n');
        let mut writer = writer.lock().unwrap_or_else(PoisonError::into_inner);
        // A line that can't be written has nowhere else to go.
        drop(writer.write_all(&line));
    })
}

/// Sends every record to `all`, and those at or above `from` to `some` as well.
#[must_use]
pub fn tee(all: Sink, some: Sink, from: Level) -> Sink {
    Box::new(move |record| {
        all(record);
        if record.level >= from {
            some(record);
        }
    })
}

// Not `weacon-testing`: it depends on this crate, so its sinks would record other types.
#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    const TIME: u64 = 1_762_077_600_000;

    pub(crate) type Records = Arc<Mutex<Vec<Record>>>;

    pub(crate) fn capture(level: Level) -> (Logger, Records) {
        capture_at(level, Box::new(|| UNIX_EPOCH + Duration::from_millis(TIME)))
    }

    fn capture_at(level: Level, now: Clock) -> (Logger, Records) {
        let (sink, records) = recording_sink();
        (Logger::new(LoggerOptions { sink, now, level }), records)
    }

    fn recording_sink() -> (Sink, Records) {
        let records = Records::default();
        let sink_records = Arc::clone(&records);
        let sink: Sink = Box::new(move |record: &Record| {
            sink_records.lock().unwrap().push(record.clone());
        });
        (sink, records)
    }

    fn messages(records: &Records) -> Vec<&'static str> {
        records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.msg)
            .collect()
    }

    fn json(records: &Mutex<Vec<Record>>) -> Value {
        serde_json::to_value(&*records.lock().unwrap()).unwrap()
    }

    #[test]
    fn writes_records_at_or_above_the_level() {
        let (log, records) = capture(Level::Warn);

        log.info("ignored", &[]);
        log.warn("disk low", &[("freeBytes", 10.into())]);
        log.fatal("crashed", &[]);

        assert_eq!(
            json(&records),
            json!([
                { "time": TIME, "level": "warn", "msg": "disk low", "freeBytes": 10 },
                { "time": TIME, "level": "fatal", "msg": "crashed" },
            ])
        );
    }

    #[test]
    fn defaults_to_info() {
        assert_eq!(Level::default(), Level::Info);
    }

    #[test]
    fn children_add_their_bindings_to_every_record() {
        let (log, records) = capture(Level::Info);
        let log = log.child(&[("app", "core".into())]);

        log.child(&[("runId", "run_1".into())])
            .child(&[("chatId", "chat_1".into())])
            .info("run started", &[]);
        log.info("idle", &[]);

        assert_eq!(
            json(&records),
            json!([
                { "time": TIME, "level": "info", "msg": "run started", "app": "core", "runId": "run_1", "chatId": "chat_1" },
                { "time": TIME, "level": "info", "msg": "idle", "app": "core" },
            ])
        );
    }

    #[test]
    fn redacts_fields_and_bindings() {
        let (log, records) = capture(Level::Info);
        let error = std::io::Error::other("MARKER-ERROR");

        log.child(&[("token", "MARKER-SECRET".into())])
            .child(&[("password", "MARKER-SECRET".into())])
            .error(
                "send failed",
                &[
                    ("text", "MARKER-CONTENT".into()),
                    ("err", Field::error(&error)),
                ],
            );

        let out = json(&records);
        assert!(!out.to_string().contains("MARKER"), "{out}");
        assert_eq!(out[0]["err"]["kind"], "Other");
    }

    #[test]
    fn fields_cannot_replace_the_time_level_or_message() {
        let (log, records) = capture(Level::Info);

        log.info(
            "real",
            &[
                ("time", "fake".into()),
                ("level", "fatal".into()),
                ("msg", "fake".into()),
            ],
        );

        assert_eq!(
            json(&records),
            json!([{ "time": TIME, "level": "info", "msg": "real" }])
        );
    }

    #[test]
    fn a_clock_before_the_epoch_gives_time_zero() {
        let (log, records) = capture_at(
            Level::Info,
            Box::new(|| UNIX_EPOCH - Duration::from_secs(1)),
        );

        log.info("early", &[]);

        assert_eq!(records.lock().unwrap()[0].time, 0);
    }

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn json_lines_writes_one_line_of_json_per_record() {
        let buffer = Buffer::default();
        let log = Logger::new(LoggerOptions {
            sink: json_lines(buffer.clone()),
            now: Box::new(|| UNIX_EPOCH + Duration::from_millis(TIME)),
            level: Level::Info,
        });

        log.info("one", &[]);
        log.info("two", &[("count", 2.into())]);

        let written = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            written,
            format!(
                "{{\"time\":{TIME},\"level\":\"info\",\"msg\":\"one\"}}\n{{\"count\":2,\"time\":{TIME},\"level\":\"info\",\"msg\":\"two\"}}\n"
            )
        );
    }

    #[test]
    fn tee_sends_the_quieter_records_to_one_sink_only() {
        let ((all, all_seen), (some, some_seen)) = (recording_sink(), recording_sink());
        let log = Logger::new(LoggerOptions {
            sink: tee(all, some, Level::Warn),
            now: Box::new(SystemTime::now),
            level: Level::Info,
        });
        log.info("run started", &[]);
        log.warn("sync failed", &[]);
        log.error("store full", &[]);

        assert_eq!(
            messages(&all_seen),
            ["run started", "sync failed", "store full"]
        );
        assert_eq!(messages(&some_seen), ["sync failed", "store full"]);
    }
}
