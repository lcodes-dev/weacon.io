use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    /// One sign-in request, which its code and its tries belong to; 1–64 ASCII letters,
    /// digits, `-` or `_`.
    ChallengeId,
    "not a challenge id"
);

/// Asks the server to send a one-time code to `email`, if it is a member of
/// `workspace`. The answer, a [`SignInStarted`], is the same either way.
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

// Emails, codes, challenges and tokens never reach a log line, even through `{:?}`.
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
    /// A member signed in for the first time.
    MemberJoined { member: MemberId },
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
        assert_eq!(
            serde_json::from_value::<WorkspaceChange>(json!({ "kind": "renamed" })).unwrap(),
            WorkspaceChange::Newer
        );
    }
}
