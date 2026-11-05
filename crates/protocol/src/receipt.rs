use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::Code;

/// A command's id, chosen by the client and kept across reconnects and retries. Sending the
/// same id again returns the first receipt instead of repeating the work; the same id with
/// different content is refused with `request.conflict`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct RequestId(#[schemars(regex(pattern = PATTERN), length(min = 1, max = MAX_LEN))] String);

const MAX_LEN: usize = 64;

// The schema's form of `is_request_id`; a test keeps the two in step.
const PATTERN: &str = r"^[A-Za-z0-9_-]+$";

impl RequestId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RequestId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if is_request_id(&id) {
            Ok(Self(id))
        } else {
            Err(id)
        }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a request id"))
    }
}

fn is_request_id(id: &str) -> bool {
    (1..=MAX_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Where a command stands. It ends `completed`, `interrupted`, `failed` or `unknown`; a command
/// whose end is unknown, such as one cut off by a restart, is never sent again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    Accepted,
    Completed,
    Interrupted,
    Failed { code: Code },
    Unknown,
}

/// The daemon's word on a command, sent when it is accepted and again when it ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Receipt {
    pub request: RequestId,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Envelope, Message, code};

    fn request(id: &str) -> RequestId {
        RequestId::try_from(id.to_owned()).unwrap()
    }

    #[test]
    fn request_ids_are_short_plain_tokens() {
        let pattern = regex::Regex::new(PATTERN).unwrap();
        let longest = "a".repeat(MAX_LEN);
        for good in ["01JC2YQ7Z8", "9b1d-4c2e_f", longest.as_str()] {
            assert!(is_request_id(good), "{good}");
            assert!(pattern.is_match(good), "{good}");
        }
        let too_long = "a".repeat(MAX_LEN + 1);
        for bad in ["", "a b", "a/b", "é", too_long.as_str()] {
            assert!(!is_request_id(bad), "{bad}");
        }
        assert_eq!(RequestId::try_from("a b".to_owned()), Err("a b".to_owned()));
        assert_eq!(request("r-1").as_str(), "r-1");
        serde_json::from_value::<RequestId>(json!("a b")).unwrap_err();
    }

    #[test]
    fn receipts_have_a_flat_tagged_shape() {
        let cases = [
            (
                Outcome::Accepted,
                json!({ "id": 3, "type": "receipt", "request": "r-1", "state": "accepted" }),
            ),
            (
                Outcome::Completed,
                json!({ "id": 3, "type": "receipt", "request": "r-1", "state": "completed" }),
            ),
            (
                Outcome::Interrupted,
                json!({ "id": 3, "type": "receipt", "request": "r-1", "state": "interrupted" }),
            ),
            (
                Outcome::Failed {
                    code: code!("request.conflict"),
                },
                json!({ "id": 3, "type": "receipt", "request": "r-1", "state": "failed", "code": "request.conflict" }),
            ),
            (
                Outcome::Unknown,
                json!({ "id": 3, "type": "receipt", "request": "r-1", "state": "unknown" }),
            ),
        ];

        for (outcome, wire) in cases {
            let envelope = Envelope {
                id: 3,
                message: Message::Receipt(Receipt {
                    request: request("r-1"),
                    outcome,
                }),
            };

            assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
            assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), envelope);
        }
    }
}
