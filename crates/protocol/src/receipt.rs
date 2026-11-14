use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Code;
use crate::token::token_id;

token_id!(
    /// A command's id, chosen by the client and kept across reconnects and retries. Sending the
    /// same id again returns the first receipt instead of repeating the work; the same id with
    /// different content is refused with the error `request.conflict`.
    RequestId,
    "not a request id"
);

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

impl Outcome {
    pub(crate) const fn is_newer(&self) -> bool {
        matches!(self, Self::Newer)
    }
}

/// The word of the core or server that runs a command, sent when it accepts the command and
/// again when it ends.
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
    use crate::test_support::request;
    use crate::{Envelope, Message, code};

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
                    request: request(),
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
                request: request(),
                outcome: Outcome::Newer,
            })
        );
    }
}
