use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hex::is_lower_hex;
use crate::token::{checked_string, token_id};
use crate::{MachineName, MemberId, Outcome};

/// Which vendor's agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    /// An agent this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
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
    id.split('-').map(str::len).eq([8, 4, 4, 4, 12]) && id.split('-').all(is_lower_hex)
}

checked_string!(AgentSession, is_uuid, "not an agent session id");

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
        /// Stays on the machine: the core sends `wants` up in its place.
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Wants")]
        wants: Option<Wants>,
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
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// What a permission prompt's tool would do, as text the core makes of its input and every
/// reader of the chat sees: at most [`WANTS_MAX`] bytes, `cut` when some was left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Wants {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut: bool,
}

/// The most bytes of text a [`Wants`] holds.
pub const WANTS_MAX: usize = 4096;

impl Wants {
    /// `text`, cut at the last character that ends within [`WANTS_MAX`] bytes.
    #[must_use]
    pub fn of(text: &str) -> Self {
        let end = text.floor_char_boundary(WANTS_MAX);
        Self {
            text: text.get(..end).unwrap_or_default().to_owned(),
            cut: end < text.len(),
        }
    }

    /// This, cut again if it holds more than [`WANTS_MAX`] bytes.
    #[must_use]
    pub fn bounded(self) -> Self {
        let cut = Self::of(&self.text);
        Self {
            cut: self.cut || cut.cut,
            ..cut
        }
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
    /// The workspace's shared agent, under the workspace's own credential, on a machine its
    /// `owner` shares: `member` called it, and only `owner` answers its permission prompts.
    Shared { member: MemberId, owner: MemberId },
    /// An owner this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
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
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{round_trips, run};

    #[test]
    fn what_a_tool_wants_is_cut_on_a_character_within_4096_bytes_and_says_so() {
        assert_eq!(WANTS_MAX, 4096);
        let fits = "a".repeat(WANTS_MAX);
        let whole = Wants::of(&fits);
        assert_eq!((whole.text.as_str(), whole.cut), (fits.as_str(), false));
        // The é would end one byte past the limit, so it goes whole.
        let split = format!("{}é", "a".repeat(WANTS_MAX - 1));
        let cut = Wants::of(&split);
        assert_eq!((cut.text.len(), cut.cut), (WANTS_MAX - 1, true));
        assert_eq!(Wants::of(&format!("{fits}b")).text, fits);

        let long = Wants {
            text: format!("{fits}b"),
            cut: false,
        };
        assert_eq!(long.bounded(), Wants::of(&format!("{fits}b")));
        let said = Wants {
            text: "ls".to_owned(),
            cut: true,
        };
        assert_eq!(said.clone().bounded(), said);
        assert_eq!(whole.clone().bounded(), whole);
        assert_eq!(
            serde_json::to_value(Wants::of("ls")).unwrap(),
            json!({ "text": "ls" })
        );
    }

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
            round_trips(&kind, json!(name));
            assert_eq!(kind.name(), Some(name));
        }
        assert_eq!(
            serde_json::from_value::<AgentKind>(json!("amp")).unwrap(),
            AgentKind::Newer
        );
        assert_eq!(AgentKind::Newer.name(), None);
    }

    #[test]
    fn an_answer_travels_with_the_prompt_it_answers() {
        let run = "run-1".parse().unwrap();
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

        round_trips(&event, wire);
    }

    #[test]
    fn events_are_tagged_and_a_newer_one_still_parses() {
        let event = RunEvent::Finished {
            outcome: Outcome::Unknown,
        };
        let wire = json!({ "event": "finished", "outcome": { "state": "unknown" } });

        round_trips(&event, wire);
        assert_eq!(
            serde_json::from_value::<RunEvent>(json!({ "event": "thinking" })).unwrap(),
            RunEvent::Newer
        );
    }

    #[test]
    fn a_status_may_name_who_runs_the_run() {
        let member = "m-1".parse().unwrap();
        let laptop = "Ada's laptop".parse().unwrap();
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
                    owner: AgentOwner::Shared {
                        member: "m-2".parse().unwrap(),
                        owner: "m-1".parse().unwrap(),
                    },
                    machine: None,
                })),
                json!({ "run": "run-7", "state": "running", "agent": { "kind": "codex",
                        "owner": { "kind": "shared", "member": "m-2", "owner": "m-1" } } }),
            ),
            (status(None), json!({ "run": "run-7", "state": "running" })),
        ];

        for (status, wire) in cases {
            round_trips(&status, wire);
        }
        let newer = |agent: Value| {
            serde_json::from_value::<RunStatus>(
                json!({ "run": "run-7", "state": "running", "agent": agent }),
            )
        };
        let robot =
            newer(json!({ "kind": "robot", "owner": { "kind": "member", "member": "m-1" } }))
                .unwrap();
        let team =
            newer(json!({ "kind": "claude", "owner": { "kind": "team", "id": 1 } })).unwrap();
        serde_json::to_value(&robot).unwrap_err();
        serde_json::to_value(&team).unwrap_err();
        assert_eq!(team.agent.map(|agent| agent.owner), Some(AgentOwner::Newer));
        newer(json!({ "kind": "claude" })).unwrap_err();
        newer(json!({ "kind": "claude", "owner": { "kind": "member" } })).unwrap_err();
        newer(json!({ "kind": "claude", "owner": { "kind": "shared" } })).unwrap_err();
        let nowhere = json!({ "kind": "claude", "machine": "",
                              "owner": { "kind": "member", "member": "m-1" } });
        newer(nowhere).unwrap_err();
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
