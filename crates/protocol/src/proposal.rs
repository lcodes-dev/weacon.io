use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{
    AgentKind, Author, DocContent, DocId, MemberId, RequestId, TaskId, TaskStatus, WorkspaceId,
};

token_id!(
    /// A proposal in a workspace, chosen by the core that proposes it and kept across
    /// reconnects, so that sending it again is the same proposal; 1–64 ASCII letters, digits,
    /// `-` or `_`.
    ProposalId,
    "not a proposal id"
);

/// What an agent asks the server for, from a core to a server that offers
/// `workspace.proposals`, for one of its owner's agents. The server answers with receipts under
/// `request`, and names the owner and machine itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalCommand {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub action: ProposalAction,
}

impl ProposalCommand {
    /// Whether it holds a part this build doesn't know, so it can only have been received.
    #[must_use]
    pub const fn is_newer(&self) -> bool {
        matches!(
            self.action,
            ProposalAction::Newer
                | ProposalAction::Propose {
                    agent: AgentKind::Newer,
                    ..
                }
                | ProposalAction::Propose {
                    change: ProposalChange::Newer,
                    ..
                }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalAction {
    /// Proposes `change`, made by an agent of `agent`'s kind, which the server can't check. It
    /// takes the place of the same machine's open proposal to the same doc or task.
    Propose {
        proposal: ProposalId,
        agent: AgentKind,
        change: ProposalChange,
    },
    /// Withdraws one of the same machine's open proposals.
    Withdraw { proposal: ProposalId },
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// What a proposal would change once a person accepts it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalChange {
    /// The doc's whole new content, written from its revision `base`.
    DocEdit {
        doc: DocId,
        base: u64,
        content: DocContent,
    },
    /// New values for the task's fields, read at its version `base`; one left out stays as it
    /// is.
    TaskEdit {
        task: TaskId,
        base: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "TaskStatus")]
        status: Option<TaskStatus>,
    },
    NewDoc {
        content: DocContent,
    },
    /// A task to do, placed last.
    NewTask {
        title: String,
    },
    /// A change this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// The doc or task a proposal is about; a new one has the id it gets once accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalAbout {
    Doc {
        doc: DocId,
    },
    Task {
        task: TaskId,
    },
    NewDoc {
        doc: DocId,
    },
    NewTask {
        task: TaskId,
    },
    /// A subject this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// Where a proposal stands. It is open until a person accepts or rejects it, the same
/// machine's newer proposal supersedes it, or that machine withdraws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProposalState {
    Open,
    /// Accepted by `by`, which made the doc or task `version`.
    Accepted {
        version: u64,
        by: MemberId,
    },
    Rejected {
        by: MemberId,
    },
    Superseded,
    Withdrawn,
    /// A state this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A proposal, from the server to clients that have the capability `workspace.proposals`, at
/// `at` seconds since the Unix epoch. Its author's machine is a name, never an id, and only an
/// open proposal still holds its `change`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalRecord {
    pub id: ProposalId,
    pub about: ProposalAbout,
    pub author: Author,
    #[serde(flatten)]
    pub state: ProposalState,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ProposalChange")]
    pub change: Option<ProposalChange>,
}

/// A workspace's proposals, newest first, from the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalList {
    pub proposals: Vec<ProposalRecord>,
}

/// A member's decision on an open proposal, from a client to the server. The answer is a
/// [`ProposalOutcome`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalDecision {
    pub verdict: ProposalVerdict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalVerdict {
    Accept,
    Reject,
    /// A verdict this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// What a [`ProposalDecision`] came to. A conflict changes nothing: the proposal stays open,
/// and the person edits the doc or task themselves, then rejects it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProposalOutcome {
    /// The doc or task is now at `version`.
    Accepted {
        version: u64,
    },
    Rejected,
    /// The doc changed since the proposal's base where the proposal changed it too: in its
    /// body, as `hunks` show, its title, or the anchors it links to.
    DocConflict {
        hunks: Vec<MergeHunk>,
        title: bool,
        anchors: bool,
    },
    /// The task's `fields` changed since the proposal's base, and the proposal changes them
    /// too.
    TaskConflict {
        fields: Vec<TaskField>,
    },
    /// An outcome this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A run of a doc's body in a [`ProposalOutcome::DocConflict`]; the hunks in order are the
/// whole body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeHunk {
    /// Lines both sides agree on.
    Same { text: String },
    /// Lines the doc and the proposal both changed since `base`, each in its own way.
    Conflict {
        base: String,
        current: String,
        proposed: String,
    },
    /// A hunk this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A task's field a proposal can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskField {
    Title,
    Status,
    /// A field this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{request, workspace};
    use crate::{AgentOwner, AgentRef};

    fn round_trip<T>(value: &T, wire: Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(&serde_json::from_value::<T>(wire).unwrap(), value);
    }

    fn content() -> DocContent {
        DocContent {
            title: "Plan".to_owned(),
            body: "Ship it.\n".to_owned(),
            anchors: None,
        }
    }

    fn propose(change: ProposalChange) -> ProposalCommand {
        ProposalCommand {
            request: request(),
            workspace: workspace(),
            action: ProposalAction::Propose {
                proposal: "p-1".parse().unwrap(),
                agent: AgentKind::Claude,
                change,
            },
        }
    }

    #[test]
    fn a_core_proposes_and_withdraws_in_tagged_commands() {
        let edit = propose(ProposalChange::TaskEdit {
            task: "t-1".parse().unwrap(),
            base: 2,
            title: None,
            status: Some(TaskStatus::Done),
        });
        let withdraw = ProposalCommand {
            request: request(),
            workspace: workspace(),
            action: ProposalAction::Withdraw {
                proposal: "p-1".parse().unwrap(),
            },
        };

        round_trip(
            &edit,
            json!({
                "request": "r-1",
                "workspace": "w-1",
                "action": {
                    "kind": "propose",
                    "proposal": "p-1",
                    "agent": "claude",
                    "change": { "kind": "task_edit", "task": "t-1", "base": 2, "status": "done" },
                },
            }),
        );
        round_trip(
            &withdraw,
            json!({
                "request": "r-1",
                "workspace": "w-1",
                "action": { "kind": "withdraw", "proposal": "p-1" },
            }),
        );
        round_trip(
            &ProposalChange::NewDoc { content: content() },
            json!({ "kind": "new_doc", "content": { "title": "Plan", "body": "Ship it.\n" } }),
        );
    }

    #[test]
    fn a_command_with_any_part_this_build_does_not_know_is_only_received() {
        let mut newer = propose(ProposalChange::NewTask {
            title: "Ship".to_owned(),
        });
        assert!(!newer.is_newer());
        for action in [
            json!({ "kind": "retract" }),
            json!({ "kind": "propose", "proposal": "p-1", "agent": "gemini",
                    "change": { "kind": "new_task", "title": "Ship" } }),
            json!({ "kind": "propose", "proposal": "p-1", "agent": "claude",
                    "change": { "kind": "new_chat" } }),
        ] {
            newer.action = serde_json::from_value(action).unwrap();
            assert!(newer.is_newer(), "{:?}", newer.action);
        }
    }

    #[test]
    fn a_record_says_where_it_stands_and_keeps_its_change_only_while_open() {
        let author = Author::Agent {
            owner: "m-1".parse().unwrap(),
            agent: Some(AgentRef {
                kind: AgentKind::Codex,
                owner: AgentOwner::Member {
                    member: "m-1".parse().unwrap(),
                },
                machine: None,
            }),
        };
        let accepted = ProposalRecord {
            id: "p-1".parse().unwrap(),
            about: ProposalAbout::NewDoc {
                doc: "d-1".parse().unwrap(),
            },
            author,
            state: ProposalState::Accepted {
                version: 1,
                by: "m-2".parse().unwrap(),
            },
            at: 5,
            change: None,
        };
        let wire = json!({
            "id": "p-1",
            "about": { "kind": "new_doc", "doc": "d-1" },
            "author": {
                "kind": "agent",
                "owner": "m-1",
                "agent": { "kind": "codex", "owner": { "kind": "member", "member": "m-1" } },
            },
            "state": "accepted",
            "version": 1,
            "by": "m-2",
            "at": 5,
        });

        round_trip(&accepted, wire);
        let open = ProposalRecord {
            state: ProposalState::Open,
            change: Some(ProposalChange::NewDoc { content: content() }),
            ..accepted
        };
        let record = serde_json::to_value(&open).unwrap();
        assert_eq!(record["state"], "open");
        assert_eq!(record["change"]["kind"], "new_doc");
    }

    #[test]
    fn a_decision_answers_with_its_outcome_or_what_conflicts() {
        round_trip(
            &ProposalDecision {
                verdict: ProposalVerdict::Accept,
            },
            json!({ "verdict": "accept" }),
        );
        round_trip(
            &ProposalOutcome::Accepted { version: 4 },
            json!({ "outcome": "accepted", "version": 4 }),
        );
        round_trip(
            &ProposalOutcome::DocConflict {
                hunks: vec![
                    MergeHunk::Same {
                        text: "# Plan\n".to_owned(),
                    },
                    MergeHunk::Conflict {
                        base: "a\n".to_owned(),
                        current: "b\n".to_owned(),
                        proposed: "c\n".to_owned(),
                    },
                ],
                title: false,
                anchors: true,
            },
            json!({
                "outcome": "doc_conflict",
                "hunks": [
                    { "kind": "same", "text": "# Plan\n" },
                    { "kind": "conflict", "base": "a\n", "current": "b\n", "proposed": "c\n" },
                ],
                "title": false,
                "anchors": true,
            }),
        );
        round_trip(
            &ProposalOutcome::TaskConflict {
                fields: vec![TaskField::Title, TaskField::Status],
            },
            json!({ "outcome": "task_conflict", "fields": ["title", "status"] }),
        );
    }

    #[test]
    fn every_part_from_a_newer_peer_still_parses() {
        assert_eq!(
            serde_json::from_value::<ProposalState>(json!({ "state": "parked" })).unwrap(),
            ProposalState::Newer
        );
        assert_eq!(
            serde_json::from_value::<ProposalAbout>(json!({ "kind": "chat" })).unwrap(),
            ProposalAbout::Newer
        );
        assert_eq!(
            serde_json::from_value::<ProposalVerdict>(json!("defer")).unwrap(),
            ProposalVerdict::Newer
        );
        assert_eq!(
            serde_json::from_value::<ProposalOutcome>(json!({ "outcome": "queued" })).unwrap(),
            ProposalOutcome::Newer
        );
        assert_eq!(
            serde_json::from_value::<MergeHunk>(json!({ "kind": "moved" })).unwrap(),
            MergeHunk::Newer
        );
        assert_eq!(
            serde_json::from_value::<TaskField>(json!("order")).unwrap(),
            TaskField::Newer
        );
        let id = serde_json::from_value::<ProposalId>(json!("../p")).unwrap_err();
        assert!(id.to_string().contains("not a proposal id"), "{id}");
    }
}
