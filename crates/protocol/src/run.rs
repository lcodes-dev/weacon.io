use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

use crate::token::token_id;
use crate::{MachineName, MemberId, Outcome};

/// Which vendor's agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    /// An agent this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl AgentKind {
    /// The vendor's name, as on the wire; `None` for an agent this build doesn't know.
    #[must_use]
    pub const fn name(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Newer => None,
        }
    }
}

/// An agent's own id for a conversation, a lowercase UUID, which resumes it later.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct AgentSession(#[schemars(regex(pattern = SESSION_PATTERN))] String);

// The schema's form of `is_uuid`; a test keeps the two in step.
const SESSION_PATTERN: &str = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

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

token_id!(
    /// The core's id for one run of an agent process, 1–64 ASCII letters, digits, `-` or `_`.
    /// Prompts carry it, so an answer only ever reaches the run that asked.
    RunId,
    "not a run id"
);

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
    /// A person answered a waiting prompt; committed before the agent is told.
    PermissionAnswered {
        answer: PermissionAnswer,
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

impl RunEvent {
    pub(crate) const fn is_newer(&self) -> bool {
        matches!(
            self,
            Self::Newer
                | Self::Finished {
                    outcome: Outcome::Newer
                }
        )
    }
}

/// One event of the run `run`, in the order the run reported it, from the core to clients that
/// have `run.stream`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunUpdate {
    pub run: RunId,
    pub event: RunEvent,
}

/// A person's answer to a [`RunEvent::PermissionAsked`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionAnswer {
    pub run: RunId,
    pub prompt: u32,
    pub allow: bool,
}

/// Where a run stands, as its chat's log records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunStatus {
    pub run: RunId,
    pub state: RunState,
    /// Who runs it; left out when not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "AgentRef")]
    pub agent: Option<AgentRef>,
}

/// An agent as people know it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRef {
    pub kind: AgentKind,
    pub owner: AgentOwner,
    /// What the machine it runs on is called, which the server stamps from its own record of
    /// the core that sent the status and never takes from the core; left out when not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "MachineName")]
    pub machine: Option<MachineName>,
}

/// Whose agent it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentOwner {
    /// A member's own, under their sign-in.
    Member { member: MemberId },
    /// The workspace's shared agent, under the workspace's own credential.
    Shared,
    /// An owner this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl AgentRef {
    /// Whether its kind or owner is one this build doesn't know.
    #[must_use]
    pub const fn is_newer(&self) -> bool {
        matches!(self.kind, AgentKind::Newer) || matches!(self.owner, AgentOwner::Newer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// The agent process is starting.
    Starting,
    /// On a turn.
    Running,
    /// On a turn, waiting for a person to answer a permission prompt.
    Waiting,
    /// Between turns, with its process alive.
    Idle,
    /// Its process was stopped; the next prompt resumes the same agent session in a new run.
    Sleeping,
    /// Over for good.
    Ended,
    /// A state this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl RunStatus {
    pub(crate) const fn is_newer(&self) -> bool {
        matches!(self.state, RunState::Newer)
            || matches!(&self.agent, Some(agent) if agent.is_newer())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::run;

    #[test]
    fn sessions_are_lowercase_uuids() {
        let pattern = regex::Regex::new(SESSION_PATTERN).unwrap();
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
    fn agent_kinds_travel_as_names_and_a_newer_one_still_parses() {
        for (kind, name) in [(AgentKind::Claude, "claude"), (AgentKind::Codex, "codex")] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(kind.name(), Some(name));
            assert_eq!(
                serde_json::from_value::<AgentKind>(json!(name)).unwrap(),
                kind
            );
        }
        assert_eq!(
            serde_json::from_value::<AgentKind>(json!("amp")).unwrap(),
            AgentKind::Newer
        );
        assert_eq!(AgentKind::Newer.name(), None);
    }

    #[test]
    fn an_answer_travels_with_the_prompt_it_answers() {
        let run = RunId::try_from("run-1".to_owned()).unwrap();
        let answer = PermissionAnswer {
            run,
            prompt: 2,
            allow: false,
        };
        let event = RunEvent::PermissionAnswered { answer };
        let wire = json!({
            "event": "permission_answered",
            "answer": { "run": "run-1", "prompt": 2, "allow": false }
        });

        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
        assert_eq!(serde_json::from_value::<RunEvent>(wire).unwrap(), event);
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

    #[test]
    fn a_status_may_name_who_runs_the_run() {
        let member = "m-1".parse().unwrap();
        let laptop = MachineName::try_from("Ada's laptop".to_owned()).unwrap();
        let status = |agent| RunStatus {
            run: run(),
            state: RunState::Running,
            agent,
        };
        let cases = [
            (
                status(Some(AgentRef {
                    kind: AgentKind::Claude,
                    owner: AgentOwner::Member { member },
                    machine: Some(laptop),
                })),
                json!({ "run": "run-7", "state": "running", "agent": { "kind": "claude",
                        "owner": { "kind": "member", "member": "m-1" }, "machine": "Ada's laptop" } }),
            ),
            (
                status(Some(AgentRef {
                    kind: AgentKind::Codex,
                    owner: AgentOwner::Shared,
                    machine: None,
                })),
                json!({ "run": "run-7", "state": "running",
                        "agent": { "kind": "codex", "owner": { "kind": "shared" } } }),
            ),
            (status(None), json!({ "run": "run-7", "state": "running" })),
        ];

        for (status, wire) in cases {
            assert_eq!(serde_json::to_value(&status).unwrap(), wire);
            assert_eq!(serde_json::from_value::<RunStatus>(wire).unwrap(), status);
            assert!(!status.is_newer());
        }
        let newer = |agent: Value| {
            serde_json::from_value::<RunStatus>(
                json!({ "run": "run-7", "state": "running", "agent": agent }),
            )
        };
        let robot = newer(json!({ "kind": "robot", "owner": { "kind": "shared" } })).unwrap();
        let team =
            newer(json!({ "kind": "claude", "owner": { "kind": "team", "id": 1 } })).unwrap();
        assert!(robot.is_newer() && team.is_newer());
        assert_eq!(team.agent.map(|agent| agent.owner), Some(AgentOwner::Newer));
        newer(json!({ "kind": "claude" })).unwrap_err();
        newer(json!({ "kind": "claude", "owner": { "kind": "member" } })).unwrap_err();
        newer(json!({ "kind": "claude", "owner": { "kind": "shared" }, "machine": "" }))
            .unwrap_err();
    }

    #[test]
    fn run_states_are_snake_case_words() {
        let states = [
            (RunState::Starting, "starting"),
            (RunState::Running, "running"),
            (RunState::Waiting, "waiting"),
            (RunState::Idle, "idle"),
            (RunState::Sleeping, "sleeping"),
            (RunState::Ended, "ended"),
        ];

        for (state, word) in states {
            let wire = RunStatus {
                run: run(),
                state,
                agent: None,
            };
            assert_eq!(
                serde_json::to_value(&wire).unwrap(),
                json!({ "run": "run-7", "state": word })
            );
        }
    }
}
