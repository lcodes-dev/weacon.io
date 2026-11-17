use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A stable 64-bit hash of code, the same on every platform and in every release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint(u64);

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;
const SEPARATOR: u8 = 0xff;

impl Fingerprint {
    /// Hashes the parts in order; parts are separated, so `["ab"]` and `["a", "b"]` differ.
    pub(crate) fn of(parts: impl IntoIterator<Item = impl AsRef<[u8]>>) -> Self {
        let mut hash = FNV_OFFSET;
        for part in parts {
            for byte in part.as_ref().iter().chain([&SEPARATOR]) {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
            }
        }
        Self(hash)
    }

    pub(crate) fn combine(fingerprints: impl IntoIterator<Item = Self>) -> Self {
        Self::of(
            fingerprints
                .into_iter()
                .map(|fingerprint| fingerprint.0.to_be_bytes()),
        )
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl FromStr for Fingerprint {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        if text.len() != 16 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(());
        }
        u64::from_str_radix(text, 16).map(Self).map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_never_change() {
        assert_eq!(Fingerprint::of([b""; 0]).to_string(), "cbf29ce484222325");
        assert_eq!(Fingerprint::of([b"a"]).to_string(), "089bc907b544c769");
    }

    #[test]
    fn parts_are_separated() {
        let joined = Fingerprint::of([b"ab"]);
        let split = Fingerprint::of([b"a", b"b"]);
        assert_ne!(joined, split);
    }

    #[test]
    fn combining_depends_on_order() {
        let (one, two) = (Fingerprint(1), Fingerprint(2));
        assert_ne!(
            Fingerprint::combine([one, two]),
            Fingerprint::combine([two, one])
        );
        assert_ne!(Fingerprint::combine([one]), Fingerprint::combine([two]));
    }

    #[test]
    fn round_trips_through_hex() {
        let fingerprint = Fingerprint(0x00ab_cdef_0123_4567);
        assert_eq!(fingerprint.to_string(), "00abcdef01234567");
        assert_eq!("00abcdef01234567".parse(), Ok(fingerprint));
        assert_eq!("00ABCDEF01234567".parse(), Ok(fingerprint));
    }

    #[test]
    fn rejects_anything_but_sixteen_hex_digits() {
        for text in [
            "",
            "abc",
            "00abcdef0123456",
            "00abcdef012345678",
            "+0abcdef01234567",
            "00abcdef0123456g",
        ] {
            assert_eq!(text.parse::<Fingerprint>(), Err(()), "{text}");
        }
    }
}
