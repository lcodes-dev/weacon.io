use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::{checked_string, token_id};
use crate::{AccountId, App, Capability, MemberId, Secret, WorkspaceId};

token_id!(
    /// A machine paired with an account, 1–64 ASCII letters, digits, `-` or `_`.
    MachineId,
    "not a machine id"
);
token_id!(
    /// One pairing of a machine, from its start to its claim; 1–64 ASCII letters, digits, `-`
    /// or `_`.
    PairingId,
    "not a pairing id"
);

/// What a machine is called: 1–64 characters, none of them control or bidirectional
/// formatting characters, so it shows the same wherever it is shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MachineName(#[schemars(regex(pattern = NAME_PATTERN), length(max = NAME_MAX))] String);

const NAME_MAX: usize = 64;

// The schema's form of `is_machine_name`; a test keeps the two in step.
const NAME_PATTERN: &str =
    r"^[^\u0000-\u001F\u007F-\u009F\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]+$";

checked_string!(MachineName, is_machine_name, "not a machine name");

fn is_machine_name(name: &str) -> bool {
    let shown = |character: char| !character.is_control() && !is_bidi_control(character);
    (1..=NAME_MAX).contains(&name.chars().count()) && name.chars().all(shown)
}

/// Whether `character` reorders the text around it, so that what is shown isn't what is stored.
fn is_bidi_control(character: char) -> bool {
    matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// The operating system a machine runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MachineOs {
    Macos,
    Linux,
    Windows,
}

/// From a core to the server, unauthenticated: starts pairing the machine. The answer is a
/// [`PairStarted`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairStart {
    pub name: MachineName,
    pub os: MachineOs,
}

/// The answer to a [`PairStart`]: the single-use `code` the person enters in the web app, and
/// the `secret` only the core that started may claim the machine with, until `expires_at`, in
/// seconds since the Unix epoch.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairStarted {
    pub pairing: PairingId,
    pub code: Secret,
    pub secret: Secret,
    pub expires_at: i64,
}

/// From the web app, signed in: the machine waiting under `code`. The answer is a
/// [`PairPending`].
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairLookup {
    pub code: Secret,
}

/// A machine waiting to be paired, for the person to confirm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairPending {
    pub pairing: PairingId,
    pub name: MachineName,
    pub os: MachineOs,
}

/// From the web app, signed in: pairs the machine waiting under `code` with the signed-in
/// account. The answer is its [`MachineSummary`].
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairConfirm {
    pub pairing: PairingId,
    pub code: Secret,
}

/// From the core that started `pairing`: whether the person has confirmed it yet. The answer
/// is a [`PairClaimed`].
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairClaim {
    pub pairing: PairingId,
    pub secret: Secret,
}

/// The answer to a [`PairClaim`].
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PairClaimed {
    /// Not confirmed yet: claim again later.
    Waiting,
    /// Paired with `owner`'s account. The core proves itself with `token`, sent as a bearer
    /// token, which every claim with the pairing's secret returns until the pairing expires or
    /// the machine is unpaired.
    Paired {
        machine: MachineId,
        owner: AccountId,
        token: Secret,
    },
}

/// A machine paired with the signed-in account, at `paired_at`, in seconds since the Unix
/// epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineSummary {
    pub id: MachineId,
    pub name: MachineName,
    pub os: MachineOs,
    pub paired_at: i64,
    /// The core's build when it last connected; none until it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "App")]
    pub core: Option<App>,
    /// What the server offers cores that this one didn't offer back when it last connected:
    /// it is older than the server supports, and those features don't reach it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<Capability>,
    /// Whether its core is connected now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub connected: bool,
}

/// The machines paired with the signed-in account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineList {
    pub machines: Vec<MachineSummary>,
}

/// The workspaces a machine's owner shares it with, all of them each time, from a core to its
/// server: when it connects, and when the set changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineShares {
    pub workspaces: Vec<WorkspaceId>,
}

/// A machine shared with a workspace, as each of its members sees it: `owner` is its owner's
/// member there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SharedMachine {
    pub id: MachineId,
    pub name: MachineName,
    pub owner: MemberId,
    /// Whether its core is connected now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub connected: bool,
}

/// The machines shared with a workspace, from the server to its members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SharedMachineList {
    pub machines: Vec<SharedMachine>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn name(name: &str) -> MachineName {
        name.parse().unwrap()
    }

    #[test]
    fn a_core_names_every_workspace_it_is_shared_with_and_members_see_a_shared_machine() {
        let shares = crate::Envelope {
            id: 3,
            message: crate::Message::MachineShares(MachineShares {
                workspaces: vec!["w-1".parse().unwrap()],
            }),
        };
        let wire = json!({ "id": 3, "type": "machine_shares", "workspaces": ["w-1"] });
        assert_eq!(serde_json::to_value(&shares).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<crate::Envelope>(wire).unwrap(),
            shares
        );
        let list = SharedMachineList {
            machines: vec![SharedMachine {
                id: "mc-1".parse().unwrap(),
                name: name("Team VPS"),
                owner: "m-1".parse().unwrap(),
                connected: true,
            }],
        };
        let wire = json!({ "machines": [{ "id": "mc-1", "name": "Team VPS", "owner": "m-1",
                                         "connected": true }] });
        assert_eq!(serde_json::to_value(&list).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<SharedMachineList>(wire).unwrap(),
            list
        );
    }

    #[test]
    fn a_name_is_short_and_shows_as_written() {
        let pattern = regex::Regex::new(NAME_PATTERN).unwrap();
        let longest = "é".repeat(64);
        for good in ["Studio", "Lena's MacBook Pro", "工作站", longest.as_str()] {
            assert_eq!(name(good).as_str(), good);
            assert!(pattern.is_match(good), "{good}");
        }
        let too_long = "a".repeat(65);
        for bad in [
            "",
            "a\nb",
            "a\u{7}",
            "a\u{202e}b",
            "\u{2066}a",
            "\u{200f}",
            "a\u{85}",
            "\u{61c}a",
            "a\u{2069}",
            too_long.as_str(),
        ] {
            assert_eq!(MachineName::try_from(bad.to_owned()), Err(bad.to_owned()));
            // The schema states the length limit apart from the pattern.
            assert!(
                bad.chars().count() > NAME_MAX || !pattern.is_match(bad),
                "{bad:?}"
            );
            let error = serde_json::from_value::<MachineName>(json!(bad)).unwrap_err();
            assert!(error.to_string().contains("not a machine name"), "{bad:?}");
        }
        assert_eq!(
            serde_json::from_value::<MachineName>(json!("Studio")).unwrap(),
            name("Studio")
        );
    }

    fn pairing() -> PairingId {
        "p-1".parse().unwrap()
    }

    #[test]
    fn ids_are_checked_on_arrival() {
        let bad = serde_json::from_value::<MachineId>(json!("a b")).unwrap_err();
        assert!(bad.to_string().contains("not a machine id"), "{bad}");
        assert_eq!("a/b".parse::<PairingId>(), Err("not a pairing id"));
    }

    #[test]
    fn pairing_bodies_travel_as_plain_json() {
        let start = PairStart {
            name: name("Studio"),
            os: MachineOs::Macos,
        };
        let start_wire = json!({ "name": "Studio", "os": "macos" });
        let claimed = PairClaimed::Paired {
            machine: "mc-1".parse().unwrap(),
            owner: "a-1".parse().unwrap(),
            token: "tok".into(),
        };
        let claimed_wire =
            json!({ "state": "paired", "machine": "mc-1", "owner": "a-1", "token": "tok" });

        assert_eq!(serde_json::to_value(&start).unwrap(), start_wire);
        assert_eq!(
            serde_json::from_value::<PairStart>(start_wire).unwrap(),
            start
        );
        assert_eq!(serde_json::to_value(&claimed).unwrap(), claimed_wire);
        assert_eq!(
            serde_json::from_value::<PairClaimed>(claimed_wire).unwrap(),
            claimed
        );
        assert_eq!(
            serde_json::to_value(PairClaimed::Waiting).unwrap(),
            json!({ "state": "waiting" })
        );
    }

    #[test]
    fn codes_secrets_and_tokens_stay_out_of_debug() {
        let secret = || Secret::from("s3cr3t-marker");
        let cases = [
            (
                format!(
                    "{:?}",
                    PairStarted {
                        pairing: pairing(),
                        code: secret(),
                        secret: secret(),
                        expires_at: 1,
                    }
                ),
                r#"PairStarted { pairing: PairingId("p-1"), code: Secret(..), secret: Secret(..), expires_at: 1 }"#,
            ),
            (
                format!("{:?}", PairLookup { code: secret() }),
                "PairLookup { code: Secret(..) }",
            ),
            (
                format!(
                    "{:?}",
                    PairConfirm {
                        pairing: pairing(),
                        code: secret()
                    }
                ),
                r#"PairConfirm { pairing: PairingId("p-1"), code: Secret(..) }"#,
            ),
            (
                format!(
                    "{:?}",
                    PairClaim {
                        pairing: pairing(),
                        secret: secret()
                    }
                ),
                r#"PairClaim { pairing: PairingId("p-1"), secret: Secret(..) }"#,
            ),
        ];

        for (shown, expected) in cases {
            assert_eq!(shown, expected);
        }
    }

    #[test]
    fn a_claimed_token_stays_out_of_debug() {
        let cases = [
            (
                format!(
                    "{:?}",
                    PairClaimed::Paired {
                        machine: "mc-1".parse().unwrap(),
                        owner: "a-1".parse().unwrap(),
                        token: "s3cr3t-marker".into(),
                    }
                ),
                r#"Paired { machine: MachineId("mc-1"), owner: AccountId("a-1"), token: Secret(..) }"#,
            ),
            (format!("{:?}", PairClaimed::Waiting), "Waiting"),
        ];

        for (shown, expected) in cases {
            assert_eq!(shown, expected);
        }
    }
}
