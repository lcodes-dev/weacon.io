use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::Code;
use crate::token::{TOKEN_MAX, TOKEN_PATTERN, is_token};

/// A command's id, chosen by the client and kept across reconnects and retries. Sending the
/// same id again returns the first receipt instead of repeating the work; the same id with
/// different content is refused with the error `request.conflict`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct RequestId(#[schemars(regex(pattern = TOKEN_PATTERN), length(max = TOKEN_MAX))] String);

impl RequestId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RequestId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if is_token(&id) { Ok(Self(id)) } else { Err(id) }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a request id"))
    }
}

/// Where an accepted command stands. It ends `completed`, `interrupted`, `failed` or `unknown`.
/// `unknown` means it may or may not have run, as when a restart cut it off; the client must not
/// send it again under a new id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    Accepted,
    Completed,
    Interrupted,
    Failed {
        code: Code,
    },
    Unknown,
    /// A state this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
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
    fn request_ids_are_plain_tokens_checked_on_arrival() {
        assert_eq!(RequestId::try_from("a b".to_owned()), Err("a b".to_owned()));
        assert_eq!(serde_json::to_value(request("r-1")).unwrap(), json!("r-1"));
        assert_eq!(request("r-1").as_str(), "r-1");
        serde_json::from_value::<RequestId>(json!("a b")).unwrap_err();
    }

    #[test]
    fn receipts_have_a_flat_tagged_shape() {
        let cases = [
            (Outcome::Accepted, json!({ "state": "accepted" })),
            (Outcome::Completed, json!({ "state": "completed" })),
            (Outcome::Interrupted, json!({ "state": "interrupted" })),
            (
                Outcome::Failed {
                    code: code!("run.failed"),
                },
                json!({ "state": "failed", "code": "run.failed" }),
            ),
            (Outcome::Unknown, json!({ "state": "unknown" })),
        ];

        for (outcome, state) in cases {
            let mut wire = json!({ "id": 3, "type": "receipt", "request": "r-1" });
            wire.as_object_mut()
                .unwrap()
                .extend(state.as_object().unwrap().clone());
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

    #[test]
    fn a_newer_peers_state_still_parses() {
        let wire = json!({ "id": 3, "type": "receipt", "request": "r-1", "state": "queued" });

        assert_eq!(
            serde_json::from_value::<Envelope>(wire).unwrap().message,
            Message::Receipt(Receipt {
                request: request("r-1"),
                outcome: Outcome::Newer,
            })
        );
    }
}
