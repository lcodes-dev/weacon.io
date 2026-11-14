use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{PermissionAnswer, Receipt, RequestId, RunStatus, RunUpdate, WorkspaceId};

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

/// Text deltas are never committed; they arrive only as run updates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatUpdate {
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub entry: ChatEntry,
}

/// An event and its place in its chat's log: `seq` starts at 1 and grows by one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatEntry {
    pub seq: u64,
    pub event: ChatEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    /// A person's prompt, accepted under `request`.
    Prompt {
        request: RequestId,
        text: String,
    },
    /// Where a chat command stands; written once per state.
    Receipt {
        receipt: Receipt,
    },
    Run {
        update: RunUpdate,
    },
    RunStatus {
        status: RunStatus,
    },
    /// An event this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// The answer is a [`ChatAnswer`], or for `subscribe` the [`ChatUpdate`]s themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatQuery {
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub query: ChatQueryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatQueryKind {
    /// Every event after `after`, then each new one as it is committed.
    Subscribe {
        after: u64,
    },
    Snapshot,
    /// The latest events before `before`: at most `limit`, and fewer when they wouldn't fit
    /// one frame.
    Window {
        before: u64,
        limit: u32,
    },
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatAnswer {
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub answer: ChatAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatAnswerKind {
    /// The client missed events and its subscription is over: fetch a snapshot, then subscribe
    /// after its `seq`.
    EventsLost,
    /// The chat as of `seq`: its runs, where in the log the permission prompts still waiting
    /// are, and its latest events, as many as fit one frame. A `window` fetches a prompt that
    /// isn't among them.
    Snapshot {
        seq: u64,
        runs: Vec<RunStatus>,
        waiting: Vec<u64>,
        events: Vec<ChatEntry>,
    },
    Window {
        events: Vec<ChatEntry>,
    },
    /// An answer this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl ChatEvent {
    pub(crate) const fn is_newer(&self) -> bool {
        match self {
            Self::Prompt { .. } => false,
            Self::Receipt { receipt } => receipt.outcome.is_newer(),
            Self::Run { update } => update.event.is_newer(),
            Self::RunStatus { status } => status.is_newer(),
            Self::Newer => true,
        }
    }
}

impl ChatAnswerKind {
    pub(crate) fn is_newer(&self) -> bool {
        match self {
            Self::EventsLost => false,
            Self::Snapshot { runs, events, .. } => {
                runs.iter().any(RunStatus::is_newer) || any_newer(events)
            }
            Self::Window { events } => any_newer(events),
            Self::Newer => true,
        }
    }
}

fn any_newer(entries: &[ChatEntry]) -> bool {
    entries.iter().any(|entry| entry.event.is_newer())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{Envelope, FrameError, Message, Outcome, RunEvent, RunId, RunState};

    fn run() -> RunId {
        RunId::try_from("run-7".to_owned()).unwrap()
    }

    fn workspace() -> WorkspaceId {
        WorkspaceId::try_from("w-1".to_owned()).unwrap()
    }

    fn chat() -> ChatId {
        ChatId::try_from("c-1".to_owned()).unwrap()
    }

    fn request() -> RequestId {
        RequestId::try_from("r-1".to_owned()).unwrap()
    }

    fn envelope(message: Message) -> Envelope {
        Envelope { id: 4, message }
    }

    fn command(action: ChatAction) -> Envelope {
        envelope(Message::ChatCommand(ChatCommand {
            request: request(),
            workspace: workspace(),
            chat: chat(),
            action,
        }))
    }

    fn update(event: ChatEvent) -> Envelope {
        envelope(Message::Chat(ChatUpdate {
            workspace: workspace(),
            chat: chat(),
            entry: ChatEntry { seq: 9, event },
        }))
    }

    fn query(query: ChatQueryKind) -> Envelope {
        envelope(Message::ChatQuery(ChatQuery {
            workspace: workspace(),
            chat: chat(),
            query,
        }))
    }

    fn answer(answer: ChatAnswerKind) -> Envelope {
        envelope(Message::ChatAnswer(ChatAnswer {
            workspace: workspace(),
            chat: chat(),
            answer,
        }))
    }

    fn idle() -> ChatEntry {
        ChatEntry {
            seq: 3,
            event: ChatEvent::RunStatus {
                status: RunStatus {
                    run: run(),
                    state: RunState::Idle,
                },
            },
        }
    }

    // The wire form of a message about the chat, with its one field of its own.
    fn about(message: &str, field: &str, body: &Value) -> Value {
        let mut wire = json!({ "id": 4, "type": message, "workspace": "w-1", "chat": "c-1" });
        wire[field] = body.clone();
        wire
    }

    fn command_wire(action: &Value) -> Value {
        let mut wire = about("chat_command", "action", action);
        wire["request"] = json!("r-1");
        wire
    }

    fn entry_wire(event: &Value) -> Value {
        about("chat", "entry", &json!({ "seq": 9, "event": event }))
    }

    fn travels(envelope: &Envelope, wire: Value) {
        assert_eq!(serde_json::to_value(envelope).unwrap(), wire);
        assert_eq!(
            Envelope::from_json(&envelope.to_json().unwrap()).unwrap(),
            *envelope
        );
        assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), *envelope);
    }

    fn never_sent(received: impl IntoIterator<Item = Value>) {
        for wire in received {
            let envelope: Envelope = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{wire}");
        }
    }

    #[test]
    fn commands_have_a_tagged_shape() {
        let answer = PermissionAnswer {
            run: run(),
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

        for (action, wire) in cases {
            travels(&command(action), command_wire(&wire));
        }
    }

    #[test]
    fn a_known_action_without_its_fields_is_refused() {
        serde_json::from_value::<Envelope>(command_wire(&json!({ "kind": "prompt" }))).unwrap_err();
    }

    #[test]
    fn events_have_a_tagged_shape_and_a_place_in_the_log() {
        let cases = [
            (
                ChatEvent::Prompt {
                    request: request(),
                    text: "Run the tests".to_owned(),
                },
                json!({ "kind": "prompt", "request": "r-1", "text": "Run the tests" }),
            ),
            (
                ChatEvent::Receipt {
                    receipt: Receipt {
                        request: request(),
                        outcome: Outcome::Accepted,
                    },
                },
                json!({ "kind": "receipt", "receipt": { "request": "r-1", "state": "accepted" } }),
            ),
            (
                ChatEvent::Run {
                    update: RunUpdate {
                        run: run(),
                        event: RunEvent::Text {
                            text: "Done.".to_owned(),
                        },
                    },
                },
                json!({ "kind": "run", "update": { "run": "run-7",
                        "event": { "event": "text", "text": "Done." } } }),
            ),
            (
                ChatEvent::RunStatus {
                    status: RunStatus {
                        run: run(),
                        state: RunState::Sleeping,
                    },
                },
                json!({ "kind": "run_status", "status": { "run": "run-7", "state": "sleeping" } }),
            ),
        ];

        for (event, wire) in cases {
            travels(&update(event), entry_wire(&wire));
        }
    }

    #[test]
    fn queries_have_a_tagged_shape() {
        let cases = [
            (
                ChatQueryKind::Subscribe { after: 7 },
                json!({ "kind": "subscribe", "after": 7 }),
            ),
            (ChatQueryKind::Snapshot, json!({ "kind": "snapshot" })),
            (
                ChatQueryKind::Window {
                    before: 7,
                    limit: 50,
                },
                json!({ "kind": "window", "before": 7, "limit": 50 }),
            ),
        ];

        for (kind, wire) in cases {
            travels(&query(kind), about("chat_query", "query", &wire));
        }
    }

    #[test]
    fn answers_have_a_tagged_shape() {
        let idle_wire = json!({ "seq": 3, "event": { "kind": "run_status",
                                "status": { "run": "run-7", "state": "idle" } } });
        let cases = [
            (ChatAnswerKind::EventsLost, json!({ "kind": "events_lost" })),
            (
                ChatAnswerKind::Snapshot {
                    seq: 3,
                    runs: vec![RunStatus {
                        run: run(),
                        state: RunState::Idle,
                    }],
                    waiting: vec![2],
                    events: vec![idle()],
                },
                json!({ "kind": "snapshot", "seq": 3,
                        "runs": [{ "run": "run-7", "state": "idle" }],
                        "waiting": [2], "events": [idle_wire] }),
            ),
            (
                ChatAnswerKind::Window {
                    events: vec![idle()],
                },
                json!({ "kind": "window", "events": [idle_wire] }),
            ),
        ];

        for (kind, wire) in cases {
            travels(&answer(kind), about("chat_answer", "answer", &wire));
        }
    }

    #[test]
    fn a_newer_peers_actions_events_and_queries_parse_and_are_never_sent() {
        let newer: Envelope =
            serde_json::from_value(command_wire(&json!({ "kind": "fork", "at": 3 }))).unwrap();
        assert_eq!(newer, command(ChatAction::Newer));

        never_sent([
            command_wire(&json!({ "kind": "fork", "at": 3 })),
            entry_wire(&json!({ "kind": "note" })),
            entry_wire(&json!({ "kind": "run_status",
                                "status": { "run": "run-7", "state": "paused" } })),
            entry_wire(&json!({ "kind": "run", "update": { "run": "run-7",
                                "event": { "event": "thinking" } } })),
            entry_wire(&json!({ "kind": "receipt",
                                "receipt": { "request": "r-1", "state": "queued" } })),
            about("chat_query", "query", &json!({ "kind": "search" })),
        ]);
    }

    #[test]
    fn a_newer_peers_answers_parse_and_are_never_sent() {
        let event = json!({ "seq": 1, "event": { "kind": "note" } });
        let state = json!({ "run": "run-7", "state": "paused" });
        let snapshot = |runs: Value, events: Value| json!({ "kind": "snapshot", "seq": 1, "runs": runs, "waiting": [], "events": events });

        never_sent(
            [
                json!({ "kind": "moved" }),
                json!({ "kind": "window", "events": [event] }),
                snapshot(json!([state]), json!([])),
                snapshot(json!([]), json!([event])),
            ]
            .iter()
            .map(|answer| about("chat_answer", "answer", answer)),
        );
    }
}
