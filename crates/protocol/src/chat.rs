use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{PermissionAnswer, RequestId, WorkspaceId};

token_id!(
    /// A chat in a workspace, 1–64 ASCII letters, digits, `-` or `_`.
    ChatId,
    "not a chat id"
);

/// The core answers with receipts under `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatCommand {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub action: ChatAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatAction {
    /// Starts a turn with `text`, or queues it behind the turn that is running.
    Prompt { text: String },
    /// Answers a permission prompt of one of the chat's runs.
    Permission { answer: PermissionAnswer },
    /// Stops the turn that is running.
    Interrupt,
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Envelope, Message, RunId};

    fn command(action: ChatAction) -> Envelope {
        Envelope {
            id: 4,
            message: Message::ChatCommand(ChatCommand {
                request: RequestId::try_from("r-1".to_owned()).unwrap(),
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                action,
            }),
        }
    }

    fn wire(action: &serde_json::Value) -> serde_json::Value {
        json!({ "id": 4, "type": "chat_command", "request": "r-1", "workspace": "w-1",
                "chat": "c-1", "action": action })
    }

    #[test]
    fn chat_ids_are_plain_tokens_checked_on_arrival() {
        assert_eq!(ChatId::try_from("a b".to_owned()), Err("a b".to_owned()));
        serde_json::from_value::<ChatId>(json!("a/b")).unwrap_err();
    }

    #[test]
    fn commands_have_a_tagged_shape() {
        let answer = PermissionAnswer {
            run: RunId::try_from("run-7".to_owned()).unwrap(),
            prompt: 2,
            allow: true,
        };
        let cases = [
            (
                ChatAction::Prompt {
                    text: "Run the tests".to_owned(),
                },
                json!({ "kind": "prompt", "text": "Run the tests" }),
            ),
            (
                ChatAction::Permission { answer },
                json!({ "kind": "permission",
                        "answer": { "run": "run-7", "prompt": 2, "allow": true } }),
            ),
            (ChatAction::Interrupt, json!({ "kind": "interrupt" })),
        ];

        for (action, action_wire) in cases {
            let envelope = command(action);
            assert_eq!(serde_json::to_value(&envelope).unwrap(), wire(&action_wire));
            assert_eq!(
                serde_json::from_value::<Envelope>(wire(&action_wire)).unwrap(),
                envelope
            );
        }
    }

    #[test]
    fn a_newer_peers_action_parses_and_is_never_sent() {
        let newer: Envelope =
            serde_json::from_value(wire(&json!({ "kind": "fork", "at": 3 }))).unwrap();

        assert_eq!(newer, command(ChatAction::Newer));
        assert_eq!(newer.to_json(), Err(crate::FrameError::Invalid));
    }

    #[test]
    fn a_known_action_without_its_fields_is_refused() {
        serde_json::from_value::<Envelope>(wire(&json!({ "kind": "prompt" }))).unwrap_err();
    }
}
