use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{Code, RequestId, RunId};

const EVENT_MAX: usize = 40;
const EVENT_WORDS: usize = 4;

// The schema's form of `is_event`; a test keeps the two in step.
const EVENT_PATTERN: &str = r"^[a-z][a-z_]*( [a-z_]+){0,3}$";

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Very noisy.
    Trace,
    /// Developer detail.
    Debug,
    /// Lifecycle.
    #[default]
    Info,
    /// Failed and handled.
    Warn,
    /// A person needs to look.
    Error,
    /// About to exit.
    Fatal,
}

/// A log record from a client that has no log of its own, such as the web app: an event name and
/// typed ids. The receiver logs only events it knows, so no other text reaches its log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientLog {
    pub level: Level,
    pub msg: LogEvent,
    // These are left out rather than null, so the schema and the generated types say so too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "RunId")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "RequestId")]
    pub request_id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Code")]
    pub code: Option<Code>,
}

/// What happened, such as `run started`: one to four words of lowercase letters or `_`, one
/// space apart, at most 40 bytes; no digits, so ids and secrets can't be spelt in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct LogEvent(#[schemars(regex(pattern = EVENT_PATTERN), length(max = EVENT_MAX))] String);

impl LogEvent {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LogEvent {
    type Error = String;

    fn try_from(event: String) -> Result<Self, Self::Error> {
        if is_event(&event) {
            Ok(Self(event))
        } else {
            Err(event)
        }
    }
}

impl<'de> Deserialize<'de> for LogEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a log event"))
    }
}

fn is_event(event: &str) -> bool {
    let word = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    };
    event.len() <= EVENT_MAX
        && event.starts_with(|first: char| first.is_ascii_lowercase())
        && event.split(' ').count() <= EVENT_WORDS
        && event.split(' ').all(word)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn events_are_short_plain_words_and_match_the_schema() {
        let pattern = regex::Regex::new(EVENT_PATTERN).unwrap();
        let longest = "a".repeat(EVENT_MAX);
        for good in [
            "app started",
            "frame_dropped",
            "report setup failed now",
            longest.as_str(),
        ] {
            assert!(is_event(good), "{good}");
            assert!(pattern.is_match(good), "{good}");
        }
        let too_long = "a".repeat(EVENT_MAX + 1);
        for bad in [
            "",
            "App started",
            "_x",
            "frame 2",
            "token a b c d",
            "run  started",
            "run started ",
            "run: x",
            too_long.as_str(),
        ] {
            assert!(!is_event(bad), "{bad}");
            assert!(bad.len() > EVENT_MAX || !pattern.is_match(bad), "{bad}");
        }
    }

    #[test]
    fn a_client_log_holds_only_its_level_event_and_ids() {
        let log: ClientLog = serde_json::from_value(
            json!({ "level": "warn", "msg": "feed lost", "run_id": "r-1", "code": "feed.lost" }),
        )
        .unwrap();

        assert_eq!(log.level, Level::Warn);
        assert_eq!(log.msg.as_str(), "feed lost");
        assert_eq!(log.request_id, None);
        assert_eq!(
            serde_json::to_value(&log).unwrap(),
            json!({ "level": "warn", "msg": "feed lost", "run_id": "r-1", "code": "feed.lost" })
        );
        for bad in [
            json!({ "level": "warn", "msg": "token 3f9a0c1d2e4b5a69" }),
            json!({ "level": "loud", "msg": "feed lost" }),
            json!({ "level": "info", "msg": "feed lost", "run_id": "a b" }),
        ] {
            serde_json::from_value::<ClientLog>(bad).unwrap_err();
        }
    }

    #[test]
    fn a_log_is_a_flat_tagged_message() {
        let wire = json!({ "id": 0, "type": "log", "level": "warn", "msg": "feed lost", "code": "feed.lost" });
        let envelope: crate::Envelope = serde_json::from_value(wire.clone()).unwrap();

        assert!(
            matches!(&envelope.message, crate::Message::Log(log) if log.code == Some(crate::code!("feed.lost")))
        );
        assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
    }

    #[test]
    fn levels_are_ordered_and_lowercase() {
        assert!(
            Level::Trace < Level::Debug
                && Level::Warn < Level::Error
                && Level::Error < Level::Fatal
        );
        assert_eq!(Level::default(), Level::Info);
        assert_eq!(serde_json::to_value(Level::Fatal).unwrap(), json!("fatal"));
    }
}
