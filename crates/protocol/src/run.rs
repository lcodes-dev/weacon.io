use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

use crate::Outcome;
use crate::token::{TOKEN_MAX, TOKEN_PATTERN, is_token};

/// An agent's own id for a conversation, a lowercase UUID, which resumes it later.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct AgentSession(#[schemars(regex(pattern = PATTERN))] String);

// The schema's form of `is_uuid`; a test keeps the two in step.
const PATTERN: &str = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

impl AgentSession {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentSession {
    type Error = String;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if is_uuid(&id) { Ok(Self(id)) } else { Err(id) }
    }
}

impl<'de> Deserialize<'de> for AgentSession {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not an agent session id"))
    }
}

/// The core's id for one run of an agent process, 1–64 ASCII letters, digits, `-` or `_`.
/// Prompts carry it, so an answer only ever reaches the run that asked.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct RunId(#[schemars(regex(pattern = TOKEN_PATTERN), length(max = TOKEN_MAX))] String);

impl TryFrom<String> for RunId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if is_token(&id) { Ok(Self(id)) } else { Err(id) }
    }
}

impl<'de> Deserialize<'de> for RunId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a run id"))
    }
}

fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(at, byte)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                matches!(byte, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

/// What an agent's run reports, in order. Each turn ends with `finished`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    /// The agent's conversation, to resume it later.
    Started {
        session: AgentSession,
    },
    /// More of the text block being written; the [`RunEvent::Text`] that follows replaces it.
    TextDelta {
        text: String,
    },
    Text {
        text: String,
    },
    ToolCall {
        call: String,
        tool: String,
        input: Value,
    },
    ToolDone {
        call: String,
        failed: bool,
    },
    /// The agent asks to use a tool and waits for a [`PermissionAnswer`].
    PermissionAsked {
        run: RunId,
        prompt: u32,
        tool: String,
        input: Value,
    },
    /// The agent no longer waits for the answer to `prompt`.
    PermissionWithdrawn {
        run: RunId,
        prompt: u32,
    },
    /// Ends the turn and withdraws any prompt still waiting.
    Finished {
        outcome: Outcome,
    },
    /// An event this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A person's answer to a [`RunEvent::PermissionAsked`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionAnswer {
    pub run: RunId,
    pub prompt: u32,
    pub allow: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sessions_are_lowercase_uuids() {
        let pattern = regex::Regex::new(PATTERN).unwrap();
        let good = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
        assert!(is_uuid(good));
        assert!(pattern.is_match(good));
        for bad in [
            "",
            "0B7C6A52-3F1E-4D2A-9C8B-5E4F3A2B1C0D",
            "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0",
            "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d0",
            "0b7c6a5203f1e-4d2a-9c8b-5e4f3a2b1c0d",
            "-b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d",
            "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0g",
        ] {
            assert!(!is_uuid(bad), "{bad}");
            assert!(!pattern.is_match(bad), "{bad}");
        }
        let session: AgentSession = serde_json::from_value(json!(good)).unwrap();
        assert_eq!(session.as_str(), good);
        assert_eq!(AgentSession::try_from("x".to_owned()), Err("x".to_owned()));
        serde_json::from_value::<AgentSession>(json!("--help")).unwrap_err();
    }

    #[test]
    fn run_ids_are_plain_tokens_checked_on_arrival() {
        let run: RunId = serde_json::from_value(json!("run-7")).unwrap();

        assert_eq!(serde_json::to_value(&run).unwrap(), json!("run-7"));
        assert_eq!(RunId::try_from("a b".to_owned()), Err("a b".to_owned()));
        serde_json::from_value::<RunId>(json!("a/b")).unwrap_err();
    }

    #[test]
    fn events_are_tagged_and_a_newer_one_still_parses() {
        let event = RunEvent::Finished {
            outcome: Outcome::Unknown,
        };
        let wire = json!({ "event": "finished", "outcome": { "state": "unknown" } });

        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
        assert_eq!(serde_json::from_value::<RunEvent>(wire).unwrap(), event);
        assert_eq!(
            serde_json::from_value::<RunEvent>(json!({ "event": "thinking" })).unwrap(),
            RunEvent::Newer
        );
    }
}
