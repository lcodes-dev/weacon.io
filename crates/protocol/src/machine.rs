use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::AccountId;
use crate::to_hex;
use crate::token::token_id;

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

/// A machine's public X25519 key, the static key of its sealed channels, as 64 lowercase hex
/// digits. A browser pins it when the person confirms the pairing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct MachineKey(#[schemars(regex(pattern = KEY_PATTERN))] String);

// The schema's form of the check in `deserialize`; a test keeps the two in step.
const KEY_PATTERN: &str = r"^[0-9a-f]{64}$";

impl MachineKey {
    #[must_use]
    pub fn from_bytes(key: [u8; 32]) -> Self {
        Self(to_hex(&key))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineKey {
    type Error = String;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        let hex = key
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if key.len() == 64 && hex {
            Ok(Self(key))
        } else {
            Err(key)
        }
    }
}

impl<'de> Deserialize<'de> for MachineKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a machine key"))
    }
}

/// What a machine is called: 1–64 characters, none of them control or bidirectional
/// formatting characters, so it shows the same wherever it is shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MachineName(#[schemars(regex(pattern = NAME_PATTERN), length(max = NAME_MAX))] String);

const NAME_MAX: usize = 64;

// The schema's form of the check in `try_from`; a test keeps the two in step.
const NAME_PATTERN: &str =
    r"^[^\u0000-\u001F\u007F-\u009F\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]+$";

impl MachineName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineName {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let shown = |character: char| {
            !character.is_control()
                && !matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        };
        if (1..=NAME_MAX).contains(&name.chars().count()) && name.chars().all(shown) {
            Ok(Self(name))
        } else {
            Err(name)
        }
    }
}

impl<'de> Deserialize<'de> for MachineName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a machine name"))
    }
}

/// The operating system a machine runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MachineOs {
    Macos,
    Linux,
    Windows,
    /// A system this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From a core to the server, unauthenticated: starts pairing the machine. The answer is a
/// [`PairStarted`]; the server refuses an `os` it doesn't know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairStart {
    pub name: MachineName,
    pub os: MachineOs,
    pub key: MachineKey,
}

/// The answer to a [`PairStart`]: the single-use `code` the person enters in the web app, and
/// the `secret` only the core that started may claim the machine with, until `expires_at`, in
/// seconds since the Unix epoch.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairStarted {
    pub pairing: PairingId,
    pub code: String,
    pub secret: String,
    pub expires_at: i64,
}

/// From the web app, signed in: the machine waiting under `code`. The answer is a
/// [`PairPending`].
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairLookup {
    pub code: String,
}

/// A machine waiting to be paired, for the person to check against what its terminal shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairPending {
    pub pairing: PairingId,
    pub name: MachineName,
    pub os: MachineOs,
    pub key: MachineKey,
}

/// From the web app, signed in: pairs the machine waiting under `code` with the signed-in
/// account. The answer is its [`MachineSummary`].
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairConfirm {
    pub pairing: PairingId,
    pub code: String,
}

/// From the core that started `pairing`: whether the person has confirmed it yet. The answer
/// is a [`PairClaimed`].
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairClaim {
    pub pairing: PairingId,
    pub secret: String,
}

/// The answer to a [`PairClaim`].
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PairClaimed {
    /// Not confirmed yet: claim again later.
    Waiting,
    /// Paired with `owner`'s account. The core proves itself with `token`, sent as a bearer
    /// token, which the server hands out only once.
    Paired {
        machine: MachineId,
        owner: AccountId,
        token: String,
    },
    /// A state this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A machine paired with the signed-in account, at `paired_at`, in seconds since the Unix
/// epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineSummary {
    pub id: MachineId,
    pub name: MachineName,
    pub os: MachineOs,
    pub key: MachineKey,
    pub paired_at: i64,
}

/// The machines paired with the signed-in account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineList {
    pub machines: Vec<MachineSummary>,
}

// Codes, secrets and tokens never reach a log line, even through `{:?}`.
impl fmt::Debug for PairStarted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairStarted")
            .field("pairing", &self.pairing)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PairLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairLookup").finish_non_exhaustive()
    }
}

impl fmt::Debug for PairConfirm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairConfirm")
            .field("pairing", &self.pairing)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PairClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairClaim")
            .field("pairing", &self.pairing)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PairClaimed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Waiting => f.write_str("Waiting"),
            Self::Paired { machine, owner, .. } => f
                .debug_struct("Paired")
                .field("machine", machine)
                .field("owner", owner)
                .finish_non_exhaustive(),
            Self::Newer => f.write_str("Newer"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn key() -> MachineKey {
        let mut bytes = [0; 32];
        bytes[0] = 0xab;
        bytes[31] = 0x0f;
        MachineKey::from_bytes(bytes)
    }

    fn name(name: &str) -> MachineName {
        MachineName::try_from(name.to_owned()).unwrap()
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
        PairingId::try_from("p-1".to_owned()).unwrap()
    }

    #[test]
    fn a_key_is_64_lowercase_hex_digits_checked_on_arrival() {
        let pattern = regex::Regex::new(KEY_PATTERN).unwrap();
        let hex = format!("ab{}0f", "0".repeat(60));

        assert!(pattern.is_match(&hex));
        assert_eq!(MachineKey::try_from(hex.clone()), Ok(key()));
        assert_eq!(key().as_str(), hex);
        assert_eq!(serde_json::to_value(key()).unwrap(), json!(hex));
        assert_eq!(
            serde_json::from_value::<MachineKey>(json!(hex)).unwrap(),
            key()
        );
        for bad in [
            hex.to_uppercase(),
            "0".repeat(62),
            format!("{hex}00"),
            format!("g{}", "0".repeat(63)),
            String::new(),
        ] {
            assert!(!pattern.is_match(&bad), "{bad}");
            assert_eq!(MachineKey::try_from(bad.clone()), Err(bad.clone()));
            let error = serde_json::from_value::<MachineKey>(json!(bad)).unwrap_err();
            assert!(error.to_string().contains("not a machine key"), "{bad}");
        }
        let bad = serde_json::from_value::<MachineId>(json!("a b")).unwrap_err();
        assert!(bad.to_string().contains("not a machine id"), "{bad}");
        assert_eq!("a/b".parse::<PairingId>(), Err("not a pairing id"));
    }

    #[test]
    fn pairing_bodies_travel_as_plain_json() {
        let start = PairStart {
            name: name("Studio"),
            os: MachineOs::Macos,
            key: key(),
        };
        let start_wire = json!({ "name": "Studio", "os": "macos", "key": key().as_str() });
        let claimed = PairClaimed::Paired {
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: AccountId::try_from("a-1".to_owned()).unwrap(),
            token: "tok".to_owned(),
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
        assert_eq!(
            serde_json::from_value::<PairClaimed>(json!({ "state": "refused" })).unwrap(),
            PairClaimed::Newer
        );
        assert_eq!(
            serde_json::from_value::<MachineOs>(json!("plan9")).unwrap(),
            MachineOs::Newer
        );
    }

    #[test]
    fn codes_secrets_and_tokens_stay_out_of_debug() {
        let secret = || "s3cr3t-marker".to_owned();
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
                r#"PairStarted { pairing: PairingId("p-1"), .. }"#,
            ),
            (
                format!("{:?}", PairLookup { code: secret() }),
                "PairLookup { .. }",
            ),
            (
                format!(
                    "{:?}",
                    PairConfirm {
                        pairing: pairing(),
                        code: secret()
                    }
                ),
                r#"PairConfirm { pairing: PairingId("p-1"), .. }"#,
            ),
            (
                format!(
                    "{:?}",
                    PairClaim {
                        pairing: pairing(),
                        secret: secret()
                    }
                ),
                r#"PairClaim { pairing: PairingId("p-1"), .. }"#,
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
                        machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
                        owner: AccountId::try_from("a-1".to_owned()).unwrap(),
                        token: "s3cr3t-marker".to_owned(),
                    }
                ),
                r#"Paired { machine: MachineId("mc-1"), owner: AccountId("a-1"), .. }"#,
            ),
            (format!("{:?}", PairClaimed::Waiting), "Waiting"),
            (format!("{:?}", PairClaimed::Newer), "Newer"),
        ];

        for (shown, expected) in cases {
            assert_eq!(shown, expected);
        }
    }
}
