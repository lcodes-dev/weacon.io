use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::chat::any_newer;
use crate::{AccountId, AgentKind, AgentSession, ChatEntry, MachineId, RequestId};

/// A person asks their own machine about the agent sessions they ran outside Weacon, from a
/// client to a core that offers `sessions.read`. No workspace holds these sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsQuery {
    pub request: RequestId,
    pub machine: MachineId,
    pub query: SessionsQueryKind,
    /// The account that asked, when the server relays the message to a machine; the core
    /// answers only its owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "AccountId")]
    pub by: Option<AccountId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionsQueryKind {
    /// The sessions in the person's own config folders, newest first.
    List,
    /// The session's events after `after`, as many as fit one frame.
    Open {
        agent: AgentKind,
        session: AgentSession,
        after: u64,
    },
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// The core's answer to a [`SessionsQuery`] under the same `request`, to a client that has
/// `sessions.read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsAnswer {
    pub request: RequestId,
    pub machine: MachineId,
    pub answer: SessionsAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionsAnswerKind {
    List {
        sessions: Vec<OutsideSession>,
    },
    /// Events in the file's order, each `seq` its place there, the agent's as runs whose id is
    /// the session's; with `more`, ask again after the last `seq`.
    Opened {
        events: Vec<ChatEntry>,
        more: bool,
    },
    /// No such session in the person's own config folders.
    Missing,
    /// An answer this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A session a person ran with their own agent outside Weacon, last changed at `modified_at`,
/// in seconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutsideSession {
    pub agent: AgentKind,
    pub session: AgentSession,
    /// The folder it ran in, when its file names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub modified_at: i64,
}

impl SessionsAnswerKind {
    pub(crate) fn is_newer(&self) -> bool {
        match self {
            Self::List { sessions } => sessions
                .iter()
                .any(|session| session.agent == AgentKind::Newer),
            Self::Opened { events, .. } => any_newer(events),
            Self::Missing => false,
            Self::Newer => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{ChatEvent, Envelope, FrameError, Message};

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn envelope(message: Message) -> Envelope {
        Envelope { id: 3, message }
    }

    fn wire(kind: &str, field: &str, body: Value) -> Value {
        let mut wire = json!({ "id": 3, "type": kind, "request": "r-1", "machine": "mc-1" });
        wire[field] = body;
        wire
    }

    fn request() -> RequestId {
        RequestId::try_from("r-1".to_owned()).unwrap()
    }

    fn machine() -> MachineId {
        MachineId::try_from("mc-1".to_owned()).unwrap()
    }

    fn session() -> OutsideSession {
        OutsideSession {
            agent: AgentKind::Claude,
            session: AgentSession::try_from(SESSION.to_owned()).unwrap(),
            folder: Some("/work/app".to_owned()),
            modified_at: 1_763_000_000,
        }
    }

    #[test]
    fn queries_and_answers_travel_as_tagged_json() {
        let query = envelope(Message::SessionsQuery(SessionsQuery {
            request: request(),
            machine: machine(),
            query: SessionsQueryKind::Open {
                agent: AgentKind::Codex,
                session: AgentSession::try_from(SESSION.to_owned()).unwrap(),
                after: 4,
            },
            by: Some(AccountId::try_from("a-1".to_owned()).unwrap()),
        }));
        let open = json!({ "kind": "open", "agent": "codex", "session": SESSION, "after": 4 });
        let mut query_wire = wire("sessions_query", "query", open);
        query_wire["by"] = json!("a-1");
        let listed = json!({ "kind": "list", "sessions": [
            { "agent": "claude", "session": SESSION, "folder": "/work/app",
              "modified_at": 1_763_000_000 },
        ] });
        let answer = envelope(Message::SessionsAnswer(SessionsAnswer {
            request: request(),
            machine: machine(),
            answer: SessionsAnswerKind::List {
                sessions: vec![session()],
            },
        }));

        for (envelope, wire) in [
            (query, query_wire),
            (answer, wire("sessions_answer", "answer", listed)),
        ] {
            assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
            assert_eq!(
                Envelope::from_json(&envelope.to_json().unwrap()).unwrap(),
                envelope
            );
        }
    }

    #[test]
    fn a_relayed_query_without_an_asker_and_a_session_without_a_folder_leave_them_out() {
        let query = SessionsQuery {
            request: request(),
            machine: machine(),
            query: SessionsQueryKind::List,
            by: None,
        };
        let session = OutsideSession {
            folder: None,
            ..session()
        };

        assert_eq!(
            serde_json::to_value(&query).unwrap(),
            json!({ "request": "r-1", "machine": "mc-1", "query": { "kind": "list" } })
        );
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            json!({ "agent": "claude", "session": SESSION, "modified_at": 1_763_000_000 })
        );
    }

    #[test]
    fn an_opened_session_carries_prompts_typed_outside_weacon() {
        let answer = SessionsAnswerKind::Opened {
            events: vec![ChatEntry {
                seq: 1,
                event: ChatEvent::OutsidePrompt {
                    text: "Fix the build".to_owned(),
                },
            }],
            more: true,
        };
        let wire = json!({ "kind": "opened", "more": true, "events": [
            { "seq": 1, "event": { "kind": "outside_prompt", "text": "Fix the build" } },
        ] });

        assert_eq!(serde_json::to_value(&answer).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<SessionsAnswerKind>(wire).unwrap(),
            answer
        );
        assert!(!answer.is_newer());
        assert!(!SessionsAnswerKind::Missing.is_newer());
    }

    #[test]
    fn a_newer_peers_queries_and_answers_parse_and_are_never_sent() {
        let newer_session = json!({ "agent": "amp", "session": SESSION, "modified_at": 0 });
        let newer = [
            wire("sessions_query", "query", json!({ "kind": "search" })),
            wire("sessions_answer", "answer", json!({ "kind": "found" })),
            wire(
                "sessions_answer",
                "answer",
                json!({ "kind": "list", "sessions": [newer_session] }),
            ),
            wire(
                "sessions_answer",
                "answer",
                json!({ "kind": "opened", "more": false,
                "events": [{ "seq": 1, "event": { "kind": "thought" } }] }),
            ),
        ];
        for wire in newer {
            let envelope: Envelope = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{wire}");
        }

        let missing = wire("sessions_answer", "answer", json!({ "kind": "missing" }));
        let known: Envelope = serde_json::from_value(missing).unwrap();
        known.to_json().unwrap();
    }
}
