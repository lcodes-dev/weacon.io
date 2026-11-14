use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccessLevel, AgentKind, Asker, MachineId, RequestId, WorkspaceId};

/// From the owner of `machine` to its core, which offers `machine.settings`: what the machine
/// keeps for `workspace`. The server relays it only from the machine's owner, and the core
/// answers only when `by` is its owner; the answer is a [`MachineSettingsAnswer`] that echoes
/// `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineSettingsQuery {
    pub request: RequestId,
    pub machine: MachineId,
    pub workspace: WorkspaceId,
    pub query: MachineSettingsQueryKind,
    /// Who asked, when the server relays the message to a machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Asker")]
    pub by: Option<Asker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineSettingsQueryKind {
    /// The settings as they are now.
    Current,
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From the owner of `machine` to its core, which offers `machine.settings`: changes what the
/// machine keeps for `workspace`. The server relays it only from the machine's owner, and the
/// core acts only when `by` is its owner; the answer is a [`MachineSettingsAnswer`] that echoes
/// `request` and holds the settings after the change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineSettingsCommand {
    pub request: RequestId,
    pub machine: MachineId,
    pub workspace: WorkspaceId,
    pub action: MachineSettingsAction,
    /// Who asked, when the server relays the message to a machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Asker")]
    pub by: Option<Asker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineSettingsAction {
    /// Whether the machine sends error reports; for the whole machine, not one workspace.
    SetReports { send: bool },
    /// What `workspace`'s agents may do on the machine, and whether other people are in it;
    /// `team` is kept as it was when left out.
    SetAccess {
        level: AccessLevel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "bool")]
        team: Option<bool>,
    },
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From the core to a client that has `machine.settings`, in answer to a
/// [`MachineSettingsQuery`] or a [`MachineSettingsCommand`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineSettingsAnswer {
    pub request: RequestId,
    pub machine: MachineId,
    pub workspace: WorkspaceId,
    pub answer: MachineSettingsAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineSettingsAnswerKind {
    Current(MachineSettings),
    /// An answer this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// What a machine keeps for one workspace, and whether it sends error reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineSettings {
    /// The owner's answer to error reports; absent until they give one, when none are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "bool")]
    pub reports: Option<bool>,
    pub access: AccessLevel,
    /// Whether other people are in the workspace.
    pub team: bool,
    /// Each agent the core runs, signed in or not with the workspace's own config folder.
    pub sign_ins: Vec<AgentSignIn>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentSignIn {
    pub agent: AgentKind,
    pub state: SignInState,
}

/// Only whether an agent is signed in: never its credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignInState {
    SignedIn,
    SignedOut,
    NotInstalled,
    /// The vendor's tool didn't say.
    Unknown,
    /// A state this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl MachineSettingsAction {
    pub(crate) fn is_newer(&self) -> bool {
        matches!(
            self,
            Self::Newer
                | Self::SetAccess {
                    level: AccessLevel::Newer,
                    ..
                }
        )
    }
}

impl MachineSettingsAnswerKind {
    pub(crate) fn is_newer(&self) -> bool {
        match self {
            Self::Current(settings) => {
                matches!(settings.access, AccessLevel::Newer)
                    || settings.sign_ins.iter().any(|sign_in| {
                        matches!(sign_in.agent, AgentKind::Newer)
                            || matches!(sign_in.state, SignInState::Newer)
                    })
            }
            Self::Newer => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{envelope, request, travels, workspace};
    use crate::{Envelope, FrameError, Message};

    fn asker() -> Asker {
        Asker {
            account: "a-1".parse().unwrap(),
            member: "m-1".parse().unwrap(),
        }
    }

    fn query(query: MachineSettingsQueryKind, by: Option<Asker>) -> Envelope {
        envelope(Message::MachineSettingsQuery(MachineSettingsQuery {
            request: request(),
            machine: "mc-1".parse().unwrap(),
            workspace: workspace(),
            query,
            by,
        }))
    }

    fn command(action: MachineSettingsAction) -> Envelope {
        envelope(Message::MachineSettingsCommand(MachineSettingsCommand {
            request: request(),
            machine: "mc-1".parse().unwrap(),
            workspace: workspace(),
            action,
            by: Some(asker()),
        }))
    }

    fn answer(answer: MachineSettingsAnswerKind) -> Envelope {
        envelope(Message::MachineSettingsAnswer(MachineSettingsAnswer {
            request: request(),
            machine: "mc-1".parse().unwrap(),
            workspace: workspace(),
            answer,
        }))
    }

    fn wire(message: &str, field: &str, body: Value) -> Value {
        let mut wire = json!({ "id": 4, "type": message, "request": "r-1",
                               "machine": "mc-1", "workspace": "w-1" });
        wire[field] = body;
        wire
    }

    fn stamped(mut wire: Value) -> Value {
        wire["by"] = json!({ "account": "a-1", "member": "m-1" });
        wire
    }

    fn settings(reports: Option<bool>, sign_ins: Vec<AgentSignIn>) -> MachineSettings {
        MachineSettings {
            reports,
            access: AccessLevel::AskFirst,
            team: true,
            sign_ins,
        }
    }

    #[test]
    fn a_query_names_the_machine_and_the_workspace_and_the_server_stamps_who_asked() {
        let unstamped = wire(
            "machine_settings_query",
            "query",
            json!({ "kind": "current" }),
        );

        travels(
            &query(MachineSettingsQueryKind::Current, None),
            unstamped.clone(),
        );
        travels(
            &query(MachineSettingsQueryKind::Current, Some(asker())),
            stamped(unstamped),
        );
    }

    #[test]
    fn commands_set_reports_or_a_workspaces_access_keeping_the_team_mark_when_left_out() {
        let action = |body: Value| stamped(wire("machine_settings_command", "action", body));

        travels(
            &command(MachineSettingsAction::SetReports { send: false }),
            action(json!({ "kind": "set_reports", "send": false })),
        );
        travels(
            &command(MachineSettingsAction::SetAccess {
                level: AccessLevel::Full,
                team: Some(true),
            }),
            action(json!({ "kind": "set_access", "level": "full", "team": true })),
        );
        travels(
            &command(MachineSettingsAction::SetAccess {
                level: AccessLevel::ReadOnly,
                team: None,
            }),
            action(json!({ "kind": "set_access", "level": "read_only" })),
        );
    }

    #[test]
    fn an_answer_holds_the_settings_and_leaves_out_an_answer_not_given() {
        let sign_ins = vec![
            AgentSignIn {
                agent: AgentKind::Claude,
                state: SignInState::SignedIn,
            },
            AgentSignIn {
                agent: AgentKind::Codex,
                state: SignInState::NotInstalled,
            },
        ];
        let body = |reports: Option<bool>| {
            let mut body = json!({ "kind": "current", "access": "ask_first", "team": true,
                "sign_ins": [{ "agent": "claude", "state": "signed_in" },
                             { "agent": "codex", "state": "not_installed" }] });
            if let Some(reports) = reports {
                body["reports"] = json!(reports);
            }
            body
        };

        for reports in [None, Some(true), Some(false)] {
            travels(
                &answer(MachineSettingsAnswerKind::Current(settings(
                    reports,
                    sign_ins.clone(),
                ))),
                wire("machine_settings_answer", "answer", body(reports)),
            );
        }
        let states = ["signed_out", "unknown"]
            .map(|state| serde_json::from_value::<SignInState>(json!(state)).unwrap());
        assert_eq!(states, [SignInState::SignedOut, SignInState::Unknown]);
    }

    // What a newer peer sends: it parses, and is never sent on.
    fn newer(message: &str, field: &str, body: Value) -> Message {
        let envelope =
            Envelope::from_json(&serde_json::to_vec(&wire(message, field, body)).unwrap()).unwrap();
        assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{message}");
        envelope.message
    }

    fn newer_answer(body: Value) -> MachineSettingsAnswerKind {
        let Message::MachineSettingsAnswer(answer) =
            newer("machine_settings_answer", "answer", body)
        else {
            panic!("not an answer");
        };
        answer.answer
    }

    #[test]
    fn a_newer_query_or_action_parses_but_is_never_sent() {
        let later = json!({ "kind": "later" });

        assert!(matches!(
            newer("machine_settings_query", "query", later.clone()),
            Message::MachineSettingsQuery(MachineSettingsQuery {
                query: MachineSettingsQueryKind::Newer,
                ..
            })
        ));
        assert!(matches!(
            newer("machine_settings_command", "action", later),
            Message::MachineSettingsCommand(MachineSettingsCommand {
                action: MachineSettingsAction::Newer,
                ..
            })
        ));
        assert!(matches!(
            newer(
                "machine_settings_command",
                "action",
                json!({ "kind": "set_access", "level": "sandboxed", "team": false })
            ),
            Message::MachineSettingsCommand(MachineSettingsCommand {
                action: MachineSettingsAction::SetAccess {
                    level: AccessLevel::Newer,
                    team: Some(false)
                },
                ..
            })
        ));
    }

    #[test]
    fn an_answer_with_anything_newer_in_it_parses_but_is_never_sent() {
        let current = |access: &str, sign_ins: Value| json!({ "kind": "current", "access": access, "team": false, "sign_ins": sign_ins });
        let sign_in = |answer: MachineSettingsAnswerKind| match answer {
            MachineSettingsAnswerKind::Current(settings) => settings.sign_ins.first().cloned(),
            MachineSettingsAnswerKind::Newer => None,
        };

        assert_eq!(
            newer_answer(json!({ "kind": "later" })),
            MachineSettingsAnswerKind::Newer
        );
        assert_eq!(
            sign_in(newer_answer(current(
                "full",
                json!([{ "agent": "claude", "state": "expired" }])
            ))),
            Some(AgentSignIn {
                agent: AgentKind::Claude,
                state: SignInState::Newer
            })
        );
        assert_eq!(
            sign_in(newer_answer(current(
                "full",
                json!([{ "agent": "amp", "state": "signed_in" }])
            ))),
            Some(AgentSignIn {
                agent: AgentKind::Newer,
                state: SignInState::SignedIn
            })
        );
        let MachineSettingsAnswerKind::Current(level) =
            newer_answer(current("sandboxed", json!([])))
        else {
            panic!("not the current settings");
        };
        assert_eq!(level.access, AccessLevel::Newer);
    }
}
