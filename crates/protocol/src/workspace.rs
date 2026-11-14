use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{AccountId, ChatId, DocId, ProposalId, TaskId};

token_id!(
    /// A workspace on the server, 1–64 ASCII letters, digits, `-` or `_`.
    WorkspaceId,
    "not a workspace id"
);
token_id!(
    /// A member of a workspace, 1–64 ASCII letters, digits, `-` or `_`.
    MemberId,
    "not a member id"
);

/// Who asked for a command, stamped by the server on what it relays to a machine and never
/// taken from a client. The core acts only when `account` is its owner's, and records only
/// `member`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Asker {
    pub account: AccountId,
    pub member: MemberId,
}

/// What a member may do in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRole {
    /// Made the workspace.
    Owner,
    /// Was added to it.
    Member,
    /// A role this build doesn't know, from a newer peer; only ever received, and it allows
    /// nothing.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// Asks the server for a new workspace, which the asker owns. The answer is its
/// [`WorkspaceSummary`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCreate {
    pub name: String,
}

/// A workspace as one of its members sees it, with their `role` in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    pub role: WorkspaceRole,
}

/// The workspaces the signed-in account is a member of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceList {
    pub workspaces: Vec<WorkspaceSummary>,
}

/// What changed in a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceChange {
    /// A member opened the workspace for the first time.
    MemberJoined { member: MemberId },
    /// A task was made, at version 1, or changed. Only for clients that have the capability
    /// `workspace.tasks`, which read the tasks again to see what it now is.
    TaskChanged { task: TaskId, version: u64 },
    /// A doc was made, at version 1, or revised. Only for clients that have the capability
    /// `workspace.docs`, which read the doc again to see what it now says.
    DocChanged { doc: DocId, version: u64 },
    /// A chat was made, or its title, state or latest turn changed. Only for clients that have
    /// the capability `workspace.chats`, which read the chats again.
    ChatChanged { chat: ChatId },
    /// A proposal was made or its state changed. Only for clients that have the capability
    /// `workspace.proposals`, which read the proposals again.
    ProposalChanged { proposal: ProposalId },
    /// A change this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// One change to `workspace`, from the server to clients that have `workspace.feed`. `seq` grows
/// strictly within a workspace, and a feed opened with `since=N` sends every change after `N`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceUpdate {
    pub workspace: WorkspaceId,
    pub seq: u64,
    pub change: WorkspaceChange,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{chat, round_trips, workspace};

    #[test]
    fn an_update_carries_its_workspace_and_order() {
        let update = WorkspaceUpdate {
            workspace: workspace(),
            seq: 7,
            change: WorkspaceChange::ChatChanged { chat: chat() },
        };
        let wire = json!({
            "workspace": "w-1",
            "seq": 7,
            "change": { "kind": "chat_changed", "chat": "c-1" },
        });

        round_trips(&update, wire);
    }

    #[test]
    fn changes_are_tagged_and_a_newer_one_still_parses() {
        let changes = [
            (
                WorkspaceChange::MemberJoined {
                    member: "m-1".parse().unwrap(),
                },
                json!({ "kind": "member_joined", "member": "m-1" }),
            ),
            (
                WorkspaceChange::TaskChanged {
                    task: "t-1".parse().unwrap(),
                    version: 2,
                },
                json!({ "kind": "task_changed", "task": "t-1", "version": 2 }),
            ),
            (
                WorkspaceChange::DocChanged {
                    doc: "d-1".parse().unwrap(),
                    version: 3,
                },
                json!({ "kind": "doc_changed", "doc": "d-1", "version": 3 }),
            ),
            (
                WorkspaceChange::ChatChanged { chat: chat() },
                json!({ "kind": "chat_changed", "chat": "c-1" }),
            ),
            (
                WorkspaceChange::ProposalChanged {
                    proposal: "p-1".parse().unwrap(),
                },
                json!({ "kind": "proposal_changed", "proposal": "p-1" }),
            ),
        ];
        for (change, wire) in changes {
            round_trips(&change, wire);
        }
        assert_eq!(
            serde_json::from_value::<WorkspaceChange>(json!({ "kind": "renamed" })).unwrap(),
            WorkspaceChange::Newer
        );
    }

    #[test]
    fn workspaces_travel_with_the_members_role() {
        let summary = WorkspaceSummary {
            id: workspace(),
            name: "Personal".to_owned(),
            role: WorkspaceRole::Owner,
        };
        let wire = json!({ "id": "w-1", "name": "Personal", "role": "owner" });
        let list = WorkspaceList {
            workspaces: vec![summary.clone()],
        };
        let create = WorkspaceCreate {
            name: "Team".to_owned(),
        };

        round_trips(&summary, wire.clone());
        round_trips(&list, json!({ "workspaces": [wire] }));
        round_trips(&create, json!({ "name": "Team" }));
        round_trips(&WorkspaceRole::Owner, json!("owner"));
        round_trips(&WorkspaceRole::Member, json!("member"));
        assert_eq!(
            serde_json::from_value::<WorkspaceRole>(json!("auditor")).unwrap(),
            WorkspaceRole::Newer
        );
    }
}
