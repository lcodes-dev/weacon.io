//! This machine's pairing with a server, kept in the state folder: the server, the machine's
//! id, owner and token, and its X25519 key, the static key of its sealed channels.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use weacon_observability::Fault;
use weacon_protocol::{AccountId, MachineId, MachineKey, code};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::platform;

/// The pairing, as `machine.json` keeps it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Paired {
    /// The server's `https://` URL.
    pub(crate) server: String,
    /// The certificates the person pinned for it, as PEM, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pinned: Option<String>,
    pub(crate) machine: MachineId,
    pub(crate) owner: AccountId,
    pub(crate) token: String,
    pub(crate) key: Key,
}

/// The machine's X25519 private key, kept as 64 hex digits.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct Key([u8; 32]);

impl Key {
    /// A key made from 32 random bytes.
    pub(crate) const fn from_random(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The public half, which the server and browsers know the machine by.
    pub(crate) fn public(&self) -> MachineKey {
        MachineKey::from_bytes(self.public_bytes())
    }

    /// How a person checks the public key: the first half of its SHA-256, in lowercase hex,
    /// in groups of four.
    pub(crate) fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.public_bytes());
        let digits = hex(digest.get(..16).unwrap_or_default());
        let groups: Vec<_> = digits
            .as_bytes()
            .chunks(4)
            .map(String::from_utf8_lossy)
            .collect();
        groups.join(" ")
    }

    fn public_bytes(&self) -> [u8; 32] {
        PublicKey::from(&StaticSecret::from(self.0)).to_bytes()
    }
}

impl TryFrom<String> for Key {
    type Error = &'static str;

    fn try_from(hex: String) -> Result<Self, Self::Error> {
        bytes(&hex).map(Self).ok_or("not a machine key")
    }
}

impl From<Key> for String {
    fn from(key: Key) -> Self {
        hex(&key.0)
    }
}

fn file(state_dir: &Path) -> PathBuf {
    state_dir.join("machine.json")
}

/// The pairing, if this machine has one.
///
/// # Errors
/// When the file is there but can't be read.
pub(crate) fn read(state_dir: &Path) -> Result<Option<Paired>, Fault> {
    match fs::read(file(state_dir)) {
        Ok(json) => serde_json::from_slice(&json)
            .map(Some)
            .map_err(|error| unreadable().because(error)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(unreadable().because(error)),
    }
}

/// Keeps `paired`, in place of any pairing before it.
///
/// # Errors
/// When it can't be written.
pub(crate) fn write(state_dir: &Path, paired: &Paired) -> Result<(), Fault> {
    let json = serde_json::to_vec_pretty(paired).map_err(|error| not_saved().because(error))?;
    platform::private_dir(state_dir)
        .and_then(|()| weacon_appkit::write_whole(&file(state_dir), &json))
        .map_err(|error| not_saved().because(error))
}

/// Forgets the pairing, once the server has said the machine is unpaired.
///
/// # Errors
/// When the file is there but can't be removed.
pub(crate) fn forget(state_dir: &Path) -> Result<(), Fault> {
    match fs::remove_file(file(state_dir)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(not_saved().because(error)),
        _ => Ok(()),
    }
}

fn unreadable() -> Fault {
    Fault::new(
        code!("machine.unreadable"),
        "This machine's pairing can't be read",
    )
}

fn not_saved() -> Fault {
    Fault::new(
        code!("machine.not_saved"),
        "This machine's pairing couldn't be saved",
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xf])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

fn bytes(hex: &str) -> Option<[u8; 32]> {
    let digits: Vec<u8> = hex
        .chars()
        .map(|digit| {
            digit
                .to_digit(16)
                .and_then(|value| u8::try_from(value).ok())
        })
        .collect::<Option<_>>()?;
    let bytes: Vec<u8> = digits
        .chunks(2)
        .map(|pair| match pair {
            [high, low] => Some(high << 4 | low),
            _ => None,
        })
        .collect::<Option<_>>()?;
    bytes.try_into().ok()
}

// The token and the key never reach a log line, even through `{:?}`.
impl fmt::Debug for Paired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paired")
            .field("machine", &self.machine)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    // A published X25519 test vector: this private key has this public key.
    const SECRET: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
    const PUBLIC: &str = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";

    fn key() -> Key {
        Key::from_random(bytes(SECRET).unwrap())
    }

    fn paired() -> Paired {
        Paired {
            server: "https://weacon.example.com".to_owned(),
            pinned: None,
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: AccountId::try_from("a-1".to_owned()).unwrap(),
            token: "t0ken-marker".to_owned(),
            key: key(),
        }
    }

    #[test]
    fn a_key_gives_its_x25519_public_half_and_a_fingerprint_of_it() {
        let fingerprint = key().fingerprint();
        let expected = hex(Sha256::digest(bytes(PUBLIC).unwrap()).get(..16).unwrap());

        assert_eq!(String::from(key()), SECRET);
        assert_eq!(key().public().as_str(), PUBLIC);
        assert_eq!(fingerprint.replace(' ', ""), expected);
        assert_eq!(fingerprint.len(), 39);
        assert!(fingerprint.split(' ').all(|group| group.len() == 4));
        assert_eq!(Key::try_from("zz".repeat(32)), Err("not a machine key"));
        assert_eq!(Key::try_from("ab".repeat(31)), Err("not a machine key"));
    }

    #[test]
    fn hex_reads_back_only_32_bytes_of_hex() {
        assert_eq!(bytes(&hex(&[7; 32])), Some([7; 32]));
        assert_eq!(bytes(&"a".repeat(63)), None);
        assert_eq!(bytes(&"a".repeat(66)), None);
        assert_eq!(bytes(&format!("g{}", "a".repeat(63))), None);
    }

    #[test]
    fn a_pairing_is_kept_read_back_and_forgotten() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");

        assert_eq!(read(&state).unwrap(), None);
        write(&state, &paired()).unwrap();
        let back = read(&state).unwrap();
        forget(&state).unwrap();
        forget(&state).unwrap();

        assert_eq!(back, Some(paired()));
        assert_eq!(read(&state).unwrap(), None);
    }

    #[test]
    fn a_damaged_pairing_is_an_error_not_an_absence() {
        let dir = TempDir::new().unwrap();
        fs::write(file(&dir), "{").unwrap();

        assert_eq!(
            read(&dir).unwrap_err().code().as_str(),
            "machine.unreadable"
        );
    }

    #[test]
    fn the_token_and_key_stay_out_of_debug() {
        let shown = format!("{:?}", paired());

        assert_eq!(shown, r#"Paired { machine: MachineId("mc-1"), .. }"#);
        assert_eq!(format!("{:?}", key()), "Key(..)");
    }
}
