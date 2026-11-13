use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TaskId;
use crate::token::token_id;

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
token_id!(
    /// A person's account on the server, 1–64 ASCII letters, digits, `-` or `_`.
    AccountId,
    "not an account id"
);
token_id!(
    /// One sign-in request, which its code and its tries belong to; 1–64 ASCII letters,
    /// digits, `-` or `_`.
    ChallengeId,
    "not a challenge id"
);

/// Sign-in to one workspace, which [`AccountSignInStart`] replaces: asks the server to send
/// a one-time code to `email`, if it is a member of `workspace`. The answer, a [`SignInStarted`], is the same either way.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInStart {
    pub workspace: WorkspaceId,
    pub email: String,
}

/// The answer to a [`SignInStart`], for members and strangers alike: the challenge to finish
/// it with.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInStarted {
    pub challenge: ChallengeId,
}

/// Trades the one-time code sent to `email` for a [`SignedIn`]. `challenge` is the one its
/// [`SignInStarted`] gave; a server that hands out challenges refuses a finish without one.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInFinish {
    pub workspace: WorkspaceId,
    pub email: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<ChallengeId>,
}

/// A signed-in member and the session token that proves it, sent as a bearer token.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedIn {
    pub member: MemberId,
    pub token: String,
}

/// Asks the server to send a one-time code to `email`, if that person may sign in. The
/// answer, a [`SignInStarted`], is the same either way.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSignInStart {
    pub email: String,
}

/// Trades the one-time code sent to `email`, and the challenge its [`SignInStarted`] gave,
/// for an [`AccountSignedIn`].
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSignInFinish {
    pub email: String,
    pub code: String,
    pub challenge: ChallengeId,
}

/// The signed-in account. `token`, sent as a bearer token, is only in the answer to an
/// [`AccountSignInFinish`], for clients that keep no cookie.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSignedIn {
    pub account: AccountId,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// What a member may do in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRole {
    Owner,
    Member,
    /// A role this build doesn't know, from a newer peer; only ever received, and it allows
    /// nothing.
    #[serde(other)]
    Newer,
}

/// Asks the server for a new workspace, which the asker owns. The answer is its
/// [`WorkspaceSummary`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreate {
    pub name: String,
}

/// A workspace as one of its members sees it, with their `role` in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    pub role: WorkspaceRole,
}

/// The workspaces the signed-in account is a member of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<WorkspaceSummary>,
}

// Emails, codes, challenges and tokens never reach a log line, even through `{:?}`.
impl fmt::Debug for AccountSignInStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSignInStart").finish_non_exhaustive()
    }
}

impl fmt::Debug for AccountSignInFinish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSignInFinish")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AccountSignedIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSignedIn")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for SignInStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInStart")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for SignInStarted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInStarted").finish_non_exhaustive()
    }
}

impl fmt::Debug for SignInFinish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInFinish")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedIn")
            .field("member", &self.member)
            .finish_non_exhaustive()
    }
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
    /// A change this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// One change to `workspace`. `seq` grows strictly within a workspace, and a feed opened with
/// `since=N` sends every change after `N`.
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

    fn workspace() -> WorkspaceId {
        WorkspaceId::try_from("w-1".to_owned()).unwrap()
    }

    #[test]
    fn ids_are_plain_tokens_checked_on_arrival() {
        let member: MemberId = serde_json::from_value(json!("m_2")).unwrap();

        assert_eq!(member.as_str(), "m_2");
        assert_eq!(workspace().as_str(), "w-1");
        assert_eq!(serde_json::to_value(workspace()).unwrap(), json!("w-1"));
        assert_eq!(
            WorkspaceId::try_from("a b".to_owned()),
            Err("a b".to_owned())
        );
        assert_eq!(MemberId::try_from(String::new()), Err(String::new()));
        assert_eq!("w-1".parse(), Ok(workspace()));
        assert_eq!("a b".parse::<WorkspaceId>(), Err("not a workspace id"));
        assert_eq!("".parse::<MemberId>(), Err("not a member id"));
        let bad = serde_json::from_value::<WorkspaceId>(json!("../x")).unwrap_err();
        assert!(bad.to_string().contains("not a workspace id"), "{bad}");
        let bad = serde_json::from_value::<MemberId>(json!("a.b")).unwrap_err();
        assert!(bad.to_string().contains("not a member id"), "{bad}");
        let bad = serde_json::from_value::<AccountId>(json!("a b")).unwrap_err();
        assert!(bad.to_string().contains("not an account id"), "{bad}");
    }

    #[test]
    fn sign_in_bodies_travel_as_plain_json() {
        let challenge = ChallengeId::try_from("c-1".to_owned()).unwrap();
        let finish = |challenge| SignInFinish {
            workspace: workspace(),
            email: "ada@example.com".to_owned(),
            code: "12345678".to_owned(),
            challenge,
        };
        let wire = json!({
            "workspace": "w-1", "email": "ada@example.com", "code": "12345678", "challenge": "c-1",
        });
        let older = json!({ "workspace": "w-1", "email": "ada@example.com", "code": "12345678" });
        let started = SignInStarted {
            challenge: challenge.clone(),
        };

        assert_eq!(
            serde_json::to_value(finish(Some(challenge.clone()))).unwrap(),
            wire
        );
        assert_eq!(
            serde_json::from_value::<SignInFinish>(wire).unwrap(),
            finish(Some(challenge))
        );
        assert_eq!(
            serde_json::from_value::<SignInFinish>(older.clone()).unwrap(),
            finish(None)
        );
        assert_eq!(serde_json::to_value(finish(None)).unwrap(), older);
        assert_eq!(
            serde_json::to_value(&started).unwrap(),
            json!({ "challenge": "c-1" })
        );
        assert_eq!(
            serde_json::from_value::<SignInStarted>(json!({ "challenge": "c-1" })).unwrap(),
            started
        );
        serde_json::from_value::<SignInStarted>(json!({ "challenge": "c.1" })).unwrap_err();
        serde_json::from_value::<SignInStarted>(json!({})).unwrap_err();
    }

    #[test]
    fn debug_leaves_out_emails_codes_and_tokens() {
        let start = SignInStart {
            workspace: workspace(),
            email: "MARKER-EMAIL".to_owned(),
        };
        let finish = SignInFinish {
            workspace: workspace(),
            email: "MARKER-EMAIL".to_owned(),
            code: "MARKER-CODE".to_owned(),
            challenge: Some(ChallengeId::try_from("MARKER-CHALLENGE".to_owned()).unwrap()),
        };
        let started = SignInStarted {
            challenge: ChallengeId::try_from("MARKER-CHALLENGE".to_owned()).unwrap(),
        };
        let signed_in = SignedIn {
            member: MemberId::try_from("m-1".to_owned()).unwrap(),
            token: "MARKER-TOKEN".to_owned(),
        };
        assert_eq!(
            format!("{start:?}"),
            r#"SignInStart { workspace: WorkspaceId("w-1"), .. }"#
        );
        assert_eq!(
            format!("{finish:?}"),
            r#"SignInFinish { workspace: WorkspaceId("w-1"), .. }"#
        );
        assert_eq!(
            format!("{signed_in:?}"),
            r#"SignedIn { member: MemberId("m-1"), .. }"#
        );
        assert_eq!(format!("{started:?}"), "SignInStarted { .. }");
    }

    #[test]
    fn changes_are_tagged_and_a_newer_one_still_parses() {
        let update = WorkspaceUpdate {
            workspace: workspace(),
            seq: 7,
            change: WorkspaceChange::MemberJoined {
                member: MemberId::try_from("m-1".to_owned()).unwrap(),
            },
        };
        let wire = json!({
            "workspace": "w-1",
            "seq": 7,
            "change": { "kind": "member_joined", "member": "m-1" },
        });

        assert_eq!(serde_json::to_value(&update).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<WorkspaceUpdate>(wire).unwrap(),
            update
        );
        let changed = WorkspaceChange::TaskChanged {
            task: TaskId::try_from("t-1".to_owned()).unwrap(),
            version: 2,
        };
        let changed_wire = json!({ "kind": "task_changed", "task": "t-1", "version": 2 });

        assert_eq!(serde_json::to_value(&changed).unwrap(), changed_wire);
        assert_eq!(
            serde_json::from_value::<WorkspaceChange>(changed_wire).unwrap(),
            changed
        );
        assert_eq!(
            serde_json::from_value::<WorkspaceChange>(json!({ "kind": "renamed" })).unwrap(),
            WorkspaceChange::Newer
        );
    }

    fn account() -> AccountId {
        AccountId::try_from("a-1".to_owned()).unwrap()
    }

    #[test]
    fn account_sign_in_bodies_travel_as_plain_json() {
        let finish = AccountSignInFinish {
            email: "ada@example.com".to_owned(),
            code: "12345678".to_owned(),
            challenge: ChallengeId::try_from("c-1".to_owned()).unwrap(),
        };
        let finish_wire =
            json!({ "email": "ada@example.com", "code": "12345678", "challenge": "c-1" });
        let signed_in = |token: Option<&str>| AccountSignedIn {
            account: account(),
            email: "ada@example.com".to_owned(),
            token: token.map(str::to_owned),
        };
        let browser = json!({ "account": "a-1", "email": "ada@example.com" });
        let other = json!({ "account": "a-1", "email": "ada@example.com", "token": "t" });
        let start: AccountSignInStart =
            serde_json::from_value(json!({ "email": "ada@example.com" })).unwrap();

        assert_eq!(start.email, "ada@example.com");
        assert_eq!(
            serde_json::to_value(&start).unwrap(),
            json!({ "email": "ada@example.com" })
        );
        assert_eq!(serde_json::to_value(&finish).unwrap(), finish_wire);
        assert_eq!(
            serde_json::from_value::<AccountSignInFinish>(finish_wire).unwrap(),
            finish
        );
        let unchallenged = json!({ "email": "ada@example.com", "code": "12345678" });
        serde_json::from_value::<AccountSignInFinish>(unchallenged).unwrap_err();
        assert_eq!(serde_json::to_value(signed_in(None)).unwrap(), browser);
        assert_eq!(
            serde_json::from_value::<AccountSignedIn>(browser).unwrap(),
            signed_in(None)
        );
        assert_eq!(serde_json::to_value(signed_in(Some("t"))).unwrap(), other);
        assert_eq!(
            serde_json::from_value::<AccountSignedIn>(other).unwrap(),
            signed_in(Some("t"))
        );
    }

    #[test]
    fn debug_leaves_out_an_accounts_email_code_and_token() {
        let finish = AccountSignInFinish {
            email: "MARKER-EMAIL".to_owned(),
            code: "MARKER-CODE".to_owned(),
            challenge: ChallengeId::try_from("MARKER-CHALLENGE".to_owned()).unwrap(),
        };
        let start = AccountSignInStart {
            email: "MARKER-EMAIL".to_owned(),
        };
        let signed_in = AccountSignedIn {
            account: account(),
            email: "MARKER-EMAIL".to_owned(),
            token: Some("MARKER-TOKEN".to_owned()),
        };

        assert_eq!(format!("{start:?}"), "AccountSignInStart { .. }");
        assert_eq!(format!("{finish:?}"), "AccountSignInFinish { .. }");
        assert_eq!(
            format!("{signed_in:?}"),
            r#"AccountSignedIn { account: AccountId("a-1"), .. }"#
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

        assert_eq!(serde_json::to_value(&summary).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<WorkspaceSummary>(wire.clone()).unwrap(),
            summary
        );
        assert_eq!(
            serde_json::to_value(&list).unwrap(),
            json!({ "workspaces": [wire] })
        );
        assert_eq!(
            serde_json::from_value::<WorkspaceList>(json!({ "workspaces": [wire] })).unwrap(),
            list
        );
        assert_eq!(
            serde_json::to_value(&create).unwrap(),
            json!({ "name": "Team" })
        );
        assert_eq!(
            serde_json::from_value::<WorkspaceCreate>(json!({ "name": "Team" })).unwrap(),
            create
        );
        for (wire, role) in [
            ("owner", WorkspaceRole::Owner),
            ("member", WorkspaceRole::Member),
            ("auditor", WorkspaceRole::Newer),
        ] {
            assert_eq!(
                serde_json::from_value::<WorkspaceRole>(json!(wire)).unwrap(),
                role
            );
        }
        assert_eq!(
            serde_json::to_value(WorkspaceRole::Member).unwrap(),
            json!("member")
        );
    }
}
