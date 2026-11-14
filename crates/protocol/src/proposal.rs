use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{
    AgentKind, Author, DocContent, DocId, MemberId, RequestId, TaskEditBody, TaskId, TaskStatus,
    WorkspaceId,
};

token_id!(
    /// A proposal in a workspace, chosen by the core that proposes it and kept across
    /// reconnects: sending it again with the same change is the same proposal, while the same
    /// id with another change, or from another machine, is refused with `proposal.conflict`.
    /// 1–64 ASCII letters, digits, `-` or `_`.
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
    pub(crate) fn is_newer(&self) -> bool {
        match &self.action {
            ProposalAction::Propose { agent, change, .. } => {
                *agent == AgentKind::Newer || change.is_newer()
            }
            ProposalAction::Withdraw { .. } => false,
            ProposalAction::Newer => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalAction {
    /// Proposes `change`, made by an agent of `agent`'s kind, which the server can't check. An
    /// edit takes the place of the same machine's open edit of the same doc or task.
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
    /// At least one edit to the task, made from its version `base`.
    TaskEdit {
        task: TaskId,
        base: u64,
        edits: Vec<TaskEditBody>,
    },
    /// A doc to make, at its first revision.
    NewDoc { content: DocContent },
    /// A task to do, placed last.
    NewTask { title: String },
    /// A change this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl ProposalChange {
    fn is_newer(&self) -> bool {
        match self {
            Self::TaskEdit { edits, .. } => edits.iter().any(|edit| {
                matches!(
                    edit,
                    TaskEditBody::Newer
                        | TaskEditBody::Status {
                            status: TaskStatus::Newer
                        }
                )
            }),
            Self::DocEdit { .. } | Self::NewDoc { .. } | Self::NewTask { .. } => false,
            Self::Newer => true,
        }
    }
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
/// machine's newer edit supersedes it, or that machine withdraws it.
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
    /// Its fields sit beside the record's, so no record field may take one of their names.
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
    /// The proposal's `edits` that change what changed in the task since the proposal's base.
    TaskConflict {
        edits: Vec<TaskEditBody>,
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
    /// Lines the merge settled: the same on both sides, or changed on one only.
    Agreed { text: String },
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{envelope, never_sent, request, round_trips, travels, workspace};
    use crate::{AgentOwner, AgentRef, Envelope, Message};

    fn content() -> DocContent {
        DocContent {
            title: "Plan".to_owned(),
            body: "Ship it.\n".to_owned(),
            anchors: None,
        }
    }

    fn content_wire() -> Value {
        json!({ "title": "Plan", "body": "Ship it.\n" })
    }

    fn command(action: Value) -> Value {
        let mut wire = json!({ "id": 4, "type": "proposal_command", "request": "r-1" });
        wire["workspace"] = json!("w-1");
        wire["action"] = action;
        wire
    }

    fn propose(change: ProposalChange) -> Envelope {
        envelope(Message::ProposalCommand(ProposalCommand {
            request: request(),
            workspace: workspace(),
            action: ProposalAction::Propose {
                proposal: "p-1".parse().unwrap(),
                agent: AgentKind::Claude,
                change,
            },
        }))
    }

    fn proposed(agent: &str, change: Value) -> Value {
        let mut action = json!({ "kind": "propose", "proposal": "p-1", "agent": agent });
        action["change"] = change;
        command(action)
    }

    #[test]
    fn a_core_proposes_edits_in_tagged_commands() {
        let edit = ProposalChange::TaskEdit {
            task: "t-1".parse().unwrap(),
            base: 2,
            edits: vec![
                TaskEditBody::Title {
                    title: "Ship".to_owned(),
                },
                TaskEditBody::Status {
                    status: TaskStatus::Done,
                },
            ],
        };
        let edits = json!([
            { "kind": "title", "title": "Ship" },
            { "kind": "status", "status": "done" },
        ]);
        let revise = ProposalChange::DocEdit {
            doc: "d-1".parse().unwrap(),
            base: 3,
            content: content(),
        };
        let mut revise_wire = json!({ "kind": "doc_edit", "doc": "d-1", "base": 3 });
        revise_wire["content"] = content_wire();

        travels(
            &propose(edit),
            proposed(
                "claude",
                json!({ "kind": "task_edit", "task": "t-1", "base": 2, "edits": edits }),
            ),
        );
        travels(&propose(revise), proposed("claude", revise_wire));
    }

    #[test]
    fn a_core_proposes_new_things_and_withdraws_in_tagged_commands() {
        let new_task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        let withdraw = envelope(Message::ProposalCommand(ProposalCommand {
            request: request(),
            workspace: workspace(),
            action: ProposalAction::Withdraw {
                proposal: "p-1".parse().unwrap(),
            },
        }));

        travels(
            &propose(ProposalChange::NewDoc { content: content() }),
            proposed(
                "claude",
                json!({ "kind": "new_doc", "content": content_wire() }),
            ),
        );
        travels(
            &propose(new_task),
            proposed("claude", json!({ "kind": "new_task", "title": "Ship" })),
        );
        travels(
            &withdraw,
            command(json!({ "kind": "withdraw", "proposal": "p-1" })),
        );
    }

    #[test]
    fn a_command_with_any_part_this_build_does_not_know_is_never_sent() {
        let edits = |edit: Value| {
            let title = json!({ "kind": "title", "title": "A" });
            json!({ "kind": "task_edit", "task": "t-1", "base": 1, "edits": [title, edit] })
        };

        never_sent(command(json!({ "kind": "retract" })));
        never_sent(proposed(
            "gemini",
            json!({ "kind": "new_task", "title": "A" }),
        ));
        never_sent(proposed("claude", json!({ "kind": "new_chat" })));
        never_sent(proposed("claude", edits(json!({ "kind": "archive" }))));
        never_sent(proposed(
            "claude",
            edits(json!({ "kind": "status", "status": "blocked" })),
        ));
    }

    fn author() -> Author {
        Author::Agent {
            owner: "m-1".parse().unwrap(),
            agent: Some(AgentRef {
                kind: AgentKind::Codex,
                owner: AgentOwner::Member {
                    member: "m-1".parse().unwrap(),
                },
                machine: None,
            }),
        }
    }

    fn record(about: ProposalAbout, state: ProposalState) -> ProposalRecord {
        ProposalRecord {
            id: "p-1".parse().unwrap(),
            about,
            author: author(),
            state,
            at: 5,
            change: None,
        }
    }

    // A record's wire: its own fields, then `about` and the state's fields beside them.
    fn record_wire(about: Value, state: Value) -> Value {
        let mut record = json!({ "id": "p-1", "at": 5 });
        record["author"] = json!({ "kind": "agent", "owner": "m-1" });
        record["author"]["agent"] =
            json!({ "kind": "codex", "owner": { "kind": "member", "member": "m-1" } });
        record["about"] = about;
        if let Value::Object(fields) = state {
            for (key, value) in fields {
                record[key] = value;
            }
        }
        record
    }

    #[test]
    fn a_record_says_where_it_stands_and_keeps_its_change_only_while_open() {
        let by = || "m-2".parse().unwrap();
        let new_task = ProposalAbout::NewTask {
            task: "t-1".parse().unwrap(),
        };
        let open = ProposalRecord {
            change: Some(ProposalChange::NewTask {
                title: "Ship".to_owned(),
            }),
            ..record(new_task, ProposalState::Open)
        };
        let mut open_wire = record_wire(
            json!({ "kind": "new_task", "task": "t-1" }),
            json!({ "state": "open" }),
        );
        open_wire["change"] = json!({ "kind": "new_task", "title": "Ship" });
        let doc = |id: &str| ProposalAbout::Doc {
            doc: id.parse().unwrap(),
        };

        round_trips(&open, open_wire);
        for (record, wire) in [
            (
                record(
                    doc("d-1"),
                    ProposalState::Accepted {
                        version: 1,
                        by: by(),
                    },
                ),
                record_wire(
                    json!({ "kind": "doc", "doc": "d-1" }),
                    json!({ "state": "accepted", "version": 1, "by": "m-2" }),
                ),
            ),
            (
                record(doc("d-2"), ProposalState::Rejected { by: by() }),
                record_wire(
                    json!({ "kind": "doc", "doc": "d-2" }),
                    json!({ "state": "rejected", "by": "m-2" }),
                ),
            ),
        ] {
            round_trips(&record, wire);
        }
    }

    #[test]
    fn a_list_holds_records_that_are_open_no_more() {
        let new_doc = ProposalAbout::NewDoc {
            doc: "d-1".parse().unwrap(),
        };
        let task = ProposalAbout::Task {
            task: "t-3".parse().unwrap(),
        };
        let list = ProposalList {
            proposals: vec![
                record(new_doc, ProposalState::Withdrawn),
                record(task, ProposalState::Superseded),
            ],
        };
        let withdrawn = || json!({ "state": "withdrawn" });

        round_trips(
            &list,
            json!({ "proposals": [
                record_wire(json!({ "kind": "new_doc", "doc": "d-1" }), withdrawn()),
                record_wire(
                    json!({ "kind": "task", "task": "t-3" }),
                    json!({ "state": "superseded" }),
                ),
            ] }),
        );
    }

    #[test]
    fn a_record_in_a_state_this_build_does_not_know_still_parses() {
        let mut wire = record_wire(
            json!({ "kind": "chat", "chat": "c-1" }),
            json!({ "state": "parked", "until": 9 }),
        );

        let parsed: ProposalRecord = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(parsed, record(ProposalAbout::Newer, ProposalState::Newer));
        wire["change"] = json!({ "kind": "new_chat" });
        let parsed: ProposalRecord = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed.change, Some(ProposalChange::Newer));
    }

    #[test]
    fn a_decision_answers_with_its_outcome_or_what_conflicts() {
        for (verdict, wire) in [
            (ProposalVerdict::Accept, "accept"),
            (ProposalVerdict::Reject, "reject"),
        ] {
            round_trips(&ProposalDecision { verdict }, json!({ "verdict": wire }));
        }
        round_trips(
            &ProposalOutcome::Accepted { version: 4 },
            json!({ "outcome": "accepted", "version": 4 }),
        );
        round_trips(&ProposalOutcome::Rejected, json!({ "outcome": "rejected" }));
        let hunks = vec![
            MergeHunk::Agreed {
                text: "# Plan\n".to_owned(),
            },
            MergeHunk::Conflict {
                base: "a\n".to_owned(),
                current: "b\n".to_owned(),
                proposed: "c\n".to_owned(),
            },
        ];
        let mut conflict = json!({ "outcome": "doc_conflict", "title": false, "anchors": true });
        conflict["hunks"] = json!([
            { "kind": "agreed", "text": "# Plan\n" },
            { "kind": "conflict", "base": "a\n", "current": "b\n", "proposed": "c\n" },
        ]);
        round_trips(
            &ProposalOutcome::DocConflict {
                hunks,
                title: false,
                anchors: true,
            },
            conflict,
        );
        round_trips(
            &ProposalOutcome::TaskConflict {
                edits: vec![TaskEditBody::Move { before: None }],
            },
            json!({ "outcome": "task_conflict", "edits": [{ "kind": "move" }] }),
        );
    }

    #[test]
    fn every_part_from_a_newer_peer_still_parses() {
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
        let id = serde_json::from_value::<ProposalId>(json!("../p")).unwrap_err();
        assert!(id.to_string().contains("not a proposal id"), "{id}");
    }
}
