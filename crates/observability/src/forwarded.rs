use std::time::{Duration, SystemTime};

use weacon_protocol::{ClientLog, Code, Level};

use crate::{Field, Logger};

/// How many records a client may send at once; it earns one more each second.
const BURST: u32 = 20;

/// The events and codes a client is known to send; nothing else from it is written.
#[derive(Debug)]
pub struct Known {
    pub events: &'static [&'static str],
    pub codes: &'static [Code],
}

/// Writes one client's forwarded logs, at most [`BURST`] at once and one a second after that.
/// Only a known event is written, as the receiver's own string; a known code may go with it.
#[derive(Debug)]
pub struct Forwarded {
    known: &'static Known,
    tokens: u32,
    refilled: SystemTime,
    dropped: u64,
    told_unknown: bool,
}

impl Forwarded {
    #[must_use]
    pub fn new(known: &'static Known, now: SystemTime) -> Self {
        Self {
            known,
            tokens: BURST,
            refilled: now,
            dropped: 0,
            told_unknown: false,
        }
    }

    /// Writes `record` to `log` if it is known and within the rate.
    pub fn take(&mut self, log: &Logger, record: &ClientLog, now: SystemTime) {
        if !self.admit(log, now) {
            return;
        }
        let Some(event) = self
            .known
            .events
            .iter()
            .find(|known| **known == record.msg.as_str())
        else {
            return self.unknown(log);
        };
        let code = match &record.code {
            Some(code) if self.known.codes.contains(code) => Some(code),
            Some(_) => {
                self.unknown(log);
                None
            }
            None => None,
        };
        let field: Option<(&str, Field)> = code.map(|code| ("code", code.as_str().into()));
        match record.level {
            Level::Trace | Level::Debug => {}
            Level::Info => log.info(event, field.as_slice()),
            Level::Warn | Level::Error | Level::Fatal => log.warn(event, field.as_slice()),
        }
    }

    fn admit(&mut self, log: &Logger, now: SystemTime) -> bool {
        let earned = now
            .duration_since(self.refilled)
            .map_or(0, |since| since.as_secs());
        let tokens = u64::from(self.tokens).saturating_add(earned);
        match u32::try_from(tokens) {
            // Full, so the part of a second already earned no longer counts.
            Ok(tokens) if tokens < BURST => {
                self.tokens = tokens;
                self.refilled += Duration::from_secs(earned);
            }
            _ => {
                self.tokens = BURST;
                self.refilled = now;
            }
        }
        if self.tokens == 0 {
            if self.dropped == 0 {
                log.warn("client logs throttled", &[]);
            }
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.tokens = self.tokens.saturating_sub(1);
        if self.dropped > 0 {
            log.warn("client logs resumed", &[("count", self.dropped.into())]);
            self.dropped = 0;
        }
        true
    }

    fn unknown(&mut self, log: &Logger) {
        if !self.told_unknown {
            self.told_unknown = true;
            log.warn("client log unknown", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use serde_json::{Value, json};
    use weacon_protocol::code;

    use super::*;
    use crate::logger::tests::{Records, capture};

    static KNOWN: Known = Known {
        events: &["app started", "connection lost"],
        codes: &[code!("connection.lost")],
    };

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(seconds * 1000 + 500)
    }

    fn record(level: &str, msg: &str, more: Value) -> ClientLog {
        let mut wire = json!({ "level": level, "msg": msg });
        if let (Some(wire), Value::Object(more)) = (wire.as_object_mut(), more) {
            wire.extend(more);
        }
        serde_json::from_value(wire).unwrap()
    }

    fn written(records: &Records) -> Vec<(Level, &'static str, Value)> {
        let records = records.lock().unwrap();
        records
            .iter()
            .map(|record| {
                (
                    record.level,
                    record.msg,
                    Value::Object(record.fields.clone()),
                )
            })
            .collect()
    }

    #[test]
    fn only_known_events_and_codes_are_written_as_the_receivers_own() {
        let (log, records) = capture(Level::Trace);
        let mut forwarded = Forwarded::new(&KNOWN, at(0));
        let ids = json!({ "run_id": "sk_live_marker", "request_id": "marker-request" });
        let lost = json!({ "code": "connection.lost" });

        forwarded.take(&log, &record("info", "app started", ids), at(0));
        forwarded.take(&log, &record("warn", "connection lost", lost), at(0));
        forwarded.take(&log, &record("info", "marker word", json!({})), at(0));
        let marker_code = json!({ "code": "marker.sk_live_123" });
        forwarded.take(&log, &record("warn", "connection lost", marker_code), at(0));

        assert_eq!(
            written(&records),
            [
                (Level::Info, "app started", json!({})),
                (
                    Level::Warn,
                    "connection lost",
                    json!({ "code": "connection.lost" })
                ),
                (Level::Warn, "client log unknown", json!({})),
                (Level::Warn, "connection lost", json!({})),
            ]
        );
        let event = written(&records)[0].1;
        assert!(KNOWN.events.iter().any(|known| std::ptr::eq(*known, event)));
    }

    #[test]
    fn quiet_levels_are_dropped_and_loud_ones_written_as_warnings() {
        let (log, records) = capture(Level::Trace);
        let mut forwarded = Forwarded::new(&KNOWN, at(0));

        for level in ["trace", "debug", "info", "warn", "error", "fatal"] {
            forwarded.take(&log, &record(level, "app started", json!({})), at(0));
        }

        let levels: Vec<Level> = written(&records).iter().map(|(level, ..)| *level).collect();
        assert_eq!(levels, [Level::Info, Level::Warn, Level::Warn, Level::Warn]);
    }

    #[test]
    fn a_client_sends_a_burst_then_one_a_second_and_the_dropped_are_counted() {
        let (log, records) = capture(Level::Trace);
        let mut forwarded = Forwarded::new(&KNOWN, at(0));
        let started = record("info", "app started", json!({}));
        let count = |records: &Records, msg: &str| {
            written(records)
                .iter()
                .filter(|(_, m, _)| *m == msg)
                .count()
        };

        for _ in 0..BURST + 3 {
            forwarded.take(&log, &started, at(0));
        }
        forwarded.take(&log, &started, at(1));
        forwarded.take(&log, &started, at(1));

        let usize_burst = usize::try_from(BURST).unwrap();
        assert_eq!(count(&records, "app started"), usize_burst + 1);
        assert_eq!(
            written(&records)[usize_burst..],
            [
                (Level::Warn, "client logs throttled", json!({})),
                (Level::Warn, "client logs resumed", json!({ "count": 3 })),
                (Level::Info, "app started", json!({})),
                (Level::Warn, "client logs throttled", json!({})),
            ]
        );
    }

    #[test]
    fn idle_seconds_refill_the_burst_but_no_further() {
        let (log, records) = capture(Level::Trace);
        let mut forwarded = Forwarded::new(&KNOWN, at(0));
        let started = record("info", "app started", json!({}));

        for _ in 0..BURST {
            forwarded.take(&log, &started, at(0));
        }
        for _ in 0..=BURST {
            forwarded.take(&log, &started, at(3600));
        }
        forwarded.take(&log, &started, at(3602));
        // A clock set back earns nothing.
        forwarded.take(&log, &started, at(10));

        let started = written(&records)
            .iter()
            .filter(|(_, msg, _)| *msg == "app started")
            .count();
        assert_eq!(started, usize::try_from(BURST * 2 + 2).unwrap());
    }
}
