use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{MemberId, WorkspaceId};

token_id!(
    /// A person's account on the server, 1–64 ASCII letters, digits, `-` or `_`.
    AccountId,
    "not an account id"
);
token_id!(
    /// One sign-in request, which its code and its tries belong to; 1–64 ASCII letters,
    /// digits, `-` or `_`.
    secret ChallengeId,
    "not a challenge id"
);

/// Sign-in to one workspace, which [`AccountSignInStart`] replaces: asks the server to send a
/// one-time code to `email`, if it is a member of `workspace`. The answer, a [`SignInStarted`],
/// is the same either way.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignInStart {
    pub workspace: WorkspaceId,
    pub email: String,
}

// Emails, codes, challenges and tokens never reach a log line, even through `{:?}`.
impl fmt::Debug for SignInStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInStart")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// The answer to a [`SignInStart`], for members and strangers alike: the challenge to finish
/// it with.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignInStarted {
    pub challenge: ChallengeId,
}

/// Trades the one-time code sent to `email` for a [`SignedIn`]. `challenge` is the one its
/// [`SignInStarted`] gave; a server that hands out challenges refuses a finish without one.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignInFinish {
    pub workspace: WorkspaceId,
    pub email: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ChallengeId")]
    pub challenge: Option<ChallengeId>,
}

impl fmt::Debug for SignInFinish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInFinish")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// A signed-in member and the session token that proves it, sent as a bearer token.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignedIn {
    pub member: MemberId,
    pub token: String,
}

impl fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedIn")
            .field("member", &self.member)
            .finish_non_exhaustive()
    }
}

/// Asks the server to send a one-time code to `email`, if that person may sign in. The
/// answer, a [`SignInStarted`], is the same either way.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSignInStart {
    pub email: String,
}

impl fmt::Debug for AccountSignInStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSignInStart").finish_non_exhaustive()
    }
}

/// Trades the one-time code sent to `email`, and the challenge its [`SignInStarted`] gave,
/// for an [`AccountSignedIn`].
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSignInFinish {
    pub email: String,
    pub code: String,
    pub challenge: ChallengeId,
}

impl fmt::Debug for AccountSignInFinish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSignInFinish")
            .finish_non_exhaustive()
    }
}

/// The signed-in account. `token`, sent as a bearer token, is only in the answer to an
/// [`AccountSignInFinish`], for clients that keep no cookie.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSignedIn {
    pub account: AccountId,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub token: Option<String>,
}

impl fmt::Debug for AccountSignedIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSignedIn")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{round_trips, workspace};

    #[test]
    fn a_challenge_hides_its_value_but_other_ids_dont() {
        let challenge: ChallengeId = "c-secret".parse().unwrap();

        assert_eq!(format!("{challenge:?}"), "ChallengeId(..)");
        assert_eq!(format!("{:?}", workspace()), r#"WorkspaceId("w-1")"#);
        assert_eq!(challenge.as_str(), "c-secret");
    }

    #[test]
    fn sign_in_bodies_travel_as_plain_json() {
        let challenge: ChallengeId = "c-1".parse().unwrap();
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

        round_trips(&finish(Some(challenge)), wire);
        round_trips(&finish(None), older);
        round_trips(&started, json!({ "challenge": "c-1" }));
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
            challenge: Some("MARKER-CHALLENGE".parse().unwrap()),
        };
        let started = SignInStarted {
            challenge: "MARKER-CHALLENGE".parse().unwrap(),
        };
        let signed_in = SignedIn {
            member: "m-1".parse().unwrap(),
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
        assert_eq!(
            format!("{started:?}"),
            "SignInStarted { challenge: ChallengeId(..) }"
        );
    }

    fn account() -> AccountId {
        "a-1".parse().unwrap()
    }

    #[test]
    fn account_sign_in_bodies_travel_as_plain_json() {
        let finish = AccountSignInFinish {
            email: "ada@example.com".to_owned(),
            code: "12345678".to_owned(),
            challenge: "c-1".parse().unwrap(),
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
        let start = AccountSignInStart {
            email: "ada@example.com".to_owned(),
        };

        round_trips(&start, json!({ "email": "ada@example.com" }));
        round_trips(&finish, finish_wire);
        let unchallenged = json!({ "email": "ada@example.com", "code": "12345678" });
        serde_json::from_value::<AccountSignInFinish>(unchallenged).unwrap_err();
        round_trips(&signed_in(None), browser);
        round_trips(&signed_in(Some("t")), other);
    }

    #[test]
    fn debug_leaves_out_an_accounts_email_code_and_token() {
        let finish = AccountSignInFinish {
            email: "MARKER-EMAIL".to_owned(),
            code: "MARKER-CODE".to_owned(),
            challenge: "MARKER-CHALLENGE".parse().unwrap(),
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
}
