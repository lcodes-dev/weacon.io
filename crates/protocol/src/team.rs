use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{MemberId, Secret, WorkspaceId, WorkspaceRole};

/// A live invitation as the workspace's owner and admins see it: the member it makes, and when
/// it ends, in seconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Invitation {
    pub member: MemberId,
    pub email: Secret,
    pub role: WorkspaceRole,
    pub expires_at: i64,
}

/// A live invitation as the person it is addressed to sees it once signed in: the workspace,
/// what the one who invited them is called there, and the role it gives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationOffer {
    pub workspace: WorkspaceId,
    pub name: String,
    pub invited_by: String,
    pub role: WorkspaceRole,
    pub expires_at: i64,
}

/// Asks the server, as `POST /api/workspaces/{workspace}/members`, to change who is in the
/// workspace; for servers that offer `workspace.team`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemberCommand {
    pub action: MemberAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemberAction {
    /// Invites `email` for seven days, as a member or, by the owner, an admin. The answer is
    /// the same whether or not the address has an account.
    Invite { email: Secret, role: WorkspaceRole },
    /// Ends a live invitation.
    Revoke { member: MemberId },
    /// Ends someone else's membership; an admin may remove only members.
    Remove { member: MemberId },
    /// Makes a member an admin or an admin a member; only the owner may.
    SetRole {
        member: MemberId,
        role: WorkspaceRole,
    },
    /// Ends the asker's own membership; the owner can't.
    Leave,
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// Answers, as `POST /api/invitations`, an invitation to the signed-in account's address; for
/// servers that offer `workspace.team`. Accepting answers with the [`crate::WorkspaceSummary`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationCommand {
    pub workspace: WorkspaceId,
    pub action: InvitationAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvitationAction {
    /// Joins the workspace.
    Accept,
    /// Stops showing the invitation to its address; the workspace still sees it as live.
    Hide,
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// One letter the server leaves in its outbox, as a JSON file, for a mail relay to send. A
/// sign-in code's letter has `code`; any other has `notice` and no `code`, so a relay that
/// knows only codes refuses it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Letter {
    pub to: Secret,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub code: Option<Secret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Notice")]
    pub notice: Option<Notice>,
}

impl std::fmt::Debug for Letter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Letter")
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

/// What a letter without a code tells its address. It carries nothing anyone wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notice {
    /// The address was invited to a workspace on this server, and signs in to see it.
    Invited,
    /// A notice this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
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
    fn member_actions_are_tagged_and_a_newer_one_still_parses() {
        let actions = [
            (
                MemberAction::Invite {
                    email: "bo@example.com".into(),
                    role: WorkspaceRole::Admin,
                },
                json!({ "kind": "invite", "email": "bo@example.com", "role": "admin" }),
            ),
            (
                MemberAction::Revoke { member: member() },
                json!({ "kind": "revoke", "member": "m-1" }),
            ),
            (
                MemberAction::Remove { member: member() },
                json!({ "kind": "remove", "member": "m-1" }),
            ),
            (
                MemberAction::SetRole {
                    member: member(),
                    role: WorkspaceRole::Member,
                },
                json!({ "kind": "set_role", "member": "m-1", "role": "member" }),
            ),
            (MemberAction::Leave, json!({ "kind": "leave" })),
        ];
        for (action, wire) in actions {
            round_trips(&MemberCommand { action }, json!({ "action": wire }));
        }
        let newer = json!({ "action": { "kind": "transfer" } });
        assert_eq!(
            serde_json::from_value::<MemberCommand>(newer)
                .unwrap()
                .action,
            MemberAction::Newer
        );
    }

    #[test]
    fn invitations_travel_with_their_workspace_or_their_address() {
        let offer = InvitationOffer {
            workspace: workspace(),
            name: "Team".to_owned(),
            invited_by: "ada".to_owned(),
            role: WorkspaceRole::Member,
            expires_at: 9,
        };
        let invitation = Invitation {
            member: member(),
            email: "bo@example.com".into(),
            role: WorkspaceRole::Admin,
            expires_at: 9,
        };
        round_trips(
            &offer,
            json!({ "workspace": "w-1", "name": "Team", "invited_by": "ada", "role": "member",
                "expires_at": 9 }),
        );
        round_trips(
            &invitation,
            json!({ "member": "m-1", "email": "bo@example.com", "role": "admin", "expires_at": 9 }),
        );
        for (action, kind) in [
            (InvitationAction::Accept, "accept"),
            (InvitationAction::Hide, "hide"),
        ] {
            let command = InvitationCommand {
                workspace: workspace(),
                action,
            };
            round_trips(
                &command,
                json!({ "workspace": "w-1", "action": { "kind": kind } }),
            );
        }
        let newer = json!({ "workspace": "w-1", "action": { "kind": "snooze" } });
        assert_eq!(
            serde_json::from_value::<InvitationCommand>(newer)
                .unwrap()
                .action,
            InvitationAction::Newer
        );
    }

    #[test]
    fn a_notice_has_no_code_so_a_relay_for_codes_refuses_it() {
        #[derive(Deserialize)]
        struct OldLetter {
            #[expect(dead_code, reason = "only parsed")]
            to: String,
            #[expect(dead_code, reason = "only parsed")]
            code: String,
        }
        let code = Letter {
            to: "ada@example.com".into(),
            code: Some("ABCD-EFGH".into()),
            notice: None,
        };
        let notice = Letter {
            to: "bo@example.com".into(),
            code: None,
            notice: Some(Notice::Invited),
        };
        let notice_wire = json!({ "to": "bo@example.com", "notice": { "kind": "invited" } });

        round_trips(
            &code,
            json!({ "to": "ada@example.com", "code": "ABCD-EFGH" }),
        );
        round_trips(&notice, notice_wire.clone());
        assert!(serde_json::from_value::<OldLetter>(notice_wire).is_err());
        assert_eq!(format!("{code:?}"), "Letter { notice: None, .. }");
        let newer = json!({ "to": "bo@example.com", "notice": { "kind": "digest" } });
        assert_eq!(
            serde_json::from_value::<Letter>(newer).unwrap().notice,
            Some(Notice::Newer)
        );
    }
}
