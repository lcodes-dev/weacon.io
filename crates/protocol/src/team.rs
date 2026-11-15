use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{MemberId, Secret, WorkspaceId};

/// A live invitation as the workspace's owner sees it: its address, and when it ends, in seconds
/// since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Invitation {
    pub email: Secret,
    pub expires_at: i64,
}

/// A live invitation as the person it is addressed to sees it once signed in, which they take up
/// with `POST /api/workspaces/{workspace}/accept`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationOffer {
    pub workspace: WorkspaceId,
    pub name: String,
    pub expires_at: i64,
}

/// Asks the server, as `POST /api/workspaces/{workspace}/members`, to change who is in the
/// workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemberCommand {
    pub action: MemberAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemberAction {
    /// Invites `email` for seven days; only the owner may. The answer is the same whether or not
    /// the address has an account.
    Invite { email: Secret },
    /// Ends the live invitation to `email`; only the owner may.
    Revoke { email: Secret },
    /// Ends a member's membership; only the owner may.
    Remove { member: MemberId },
    /// Ends the asker's own membership; the owner can't.
    Leave,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{round_trips, workspace};

    fn member() -> MemberId {
        "m-1".parse().unwrap()
    }

    #[test]
    fn member_actions_are_tagged() {
        let actions = [
            (
                MemberAction::Invite {
                    email: "bo@example.com".into(),
                },
                json!({ "kind": "invite", "email": "bo@example.com" }),
            ),
            (
                MemberAction::Revoke {
                    email: "bo@example.com".into(),
                },
                json!({ "kind": "revoke", "email": "bo@example.com" }),
            ),
            (
                MemberAction::Remove { member: member() },
                json!({ "kind": "remove", "member": "m-1" }),
            ),
            (MemberAction::Leave, json!({ "kind": "leave" })),
        ];
        for (action, wire) in actions {
            round_trips(&MemberCommand { action }, json!({ "action": wire }));
        }
        let unknown = json!({ "action": { "kind": "transfer" } });
        assert!(serde_json::from_value::<MemberCommand>(unknown).is_err());
    }

    #[test]
    fn invitations_travel_with_their_workspace_or_their_address() {
        let offer = InvitationOffer {
            workspace: workspace(),
            name: "Team".to_owned(),
            expires_at: 9,
        };
        let invitation = Invitation {
            email: "bo@example.com".into(),
            expires_at: 9,
        };
        round_trips(
            &offer,
            json!({ "workspace": "w-1", "name": "Team", "expires_at": 9 }),
        );
        round_trips(
            &invitation,
            json!({ "email": "bo@example.com", "expires_at": 9 }),
        );
    }
}
