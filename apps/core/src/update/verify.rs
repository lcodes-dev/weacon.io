//! Checks a release's manifest against the keys every build trusts.

use minisign_verify::{PublicKey, Signature};
use weacon_observability::Fault;
use weacon_protocol::{ReleaseManifest, code};

/// The public keys a manifest may be signed with, in minisign's format.
pub(crate) const TRUSTED: [&str; 2] = [
    // The release key, which signs every release. Id 4D9033C9C7C1F681.
    "RWSB9sHHyTOQTbnTpivUps8H0c7vyKe+aYHpqByk9GXM84xHLYOMcUtn",
    // The backup key, kept offline for when the release key is lost or leaked. Id FC5980A8D9FB8637.
    "RWQ3hvvZqIBZ/IE1a44x0Z7bIskcOI2qzIXhZ4TLj0qxk0bLLox687ph",
];

/// The manifest in `bytes`, once `signature` shows one of `keys` signed exactly these bytes.
/// Legacy signatures, which sign the bytes rather than their hash, are refused.
pub(crate) fn verified(
    bytes: &[u8],
    signature: &str,
    keys: &[&str],
) -> Result<ReleaseManifest, Fault> {
    let unsigned = || {
        Fault::new(
            code!("update.unsigned"),
            "The release isn't signed by a key this build trusts",
        )
    };
    let signature = Signature::decode(signature).map_err(|_| unsigned())?;
    // A key that doesn't parse trusts nothing; a test checks the shipped ones.
    let trusted = keys
        .iter()
        .filter_map(|key| PublicKey::from_base64(key).ok())
        .any(|key| key.verify(bytes, &signature, false).is_ok());
    if !trusted {
        return Err(unsigned());
    }
    serde_json::from_slice(bytes).map_err(|error| {
        Fault::new(
            code!("update.bad_manifest"),
            "The release's manifest can't be read",
        )
        .because(error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Signed with a key made for these tests, whose secret half was then thrown away.
    const TEST_KEY: &str = "RWTGzWquLR59N1pXDkox+4RH+bJszZVLIB9t3Dl1O+2wU5eSZwF3H/2u";
    const MANIFEST: &str = concat!(
        r#"{"version":"1.2.3","commit":"0123456789abcdef0123456789abcdef01234567","#,
        r#""published_at":1794614400,"protocol":1,"builds":[{"#,
        r#""target":"x86_64-unknown-linux-musl","#,
        r#""url":"https://example.com/weacon-x86_64-unknown-linux-musl","size":7148880,"#,
        r#""sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}]}"#,
        "\n"
    );
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTGzWquLR59N9nG/L2FVVYwAZk/0JaJuPmJWHELg+xq0LQ5v0mzp2GS8mQdv5IMdiIwjvBE2AH700UwTtkt+wBfz3IX9fme2gY=
trusted comment: test manifest
BJKm/Twbm6UE5SrGR40+jVd8EZZraJi5+z8RHhiwSsuvp+6yA5SLFoH0nGs0iIeXHJ6pjDAnJUhn4VIqbNX2Cg==
";
    const LEGACY: &str = "untrusted comment: signature from minisign secret key
RWTGzWquLR59NySp5wMtiI0cCXW/CvvSHBMMZKeNcmAeM6FAcwNrztq5g4yF8Wq7tflpGSDKN9o0uCqMRkj4MZcCDTmhKEc8ZQg=
trusted comment: legacy
kM5rX8+bLA53QVhWe0U2A00AdVKO5EEIS4lY/tmrhGDBqTfMdieCJ/pB0AZBGtzRTYCI6fM0NyDv6rrGqg81Dw==
";
    const OTHER: &str = "{\"version\":\"1.2\"}\n";
    const OTHER_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTGzWquLR59N5g8E6w+71VtUDPj6PmSr05tM4ozgON6dRE7LQFawm58y96JnRrDrb4gUdCK7uKrW3/4hboFk6CgsuN48+gDJQE=
trusted comment: not a manifest
34luRc6UU04v1sKriVrzzEIxAoyqMofwPqhAEWIU8MhnY+I0iepcOXV45b9eihUl6556Mn16d91VUUr2Z1iOBQ==
";

    fn code(result: Result<ReleaseManifest, Fault>) -> String {
        result.unwrap_err().code().as_str().to_owned()
    }

    #[test]
    fn a_manifest_signed_by_a_trusted_key_is_read() {
        let keys = [TRUSTED[0], TEST_KEY];
        let manifest = verified(MANIFEST.as_bytes(), SIGNATURE, &keys).unwrap();
        assert_eq!(manifest.version.to_string(), "1.2.3");
    }

    #[test]
    fn the_shipped_keys_do_not_trust_a_test_signature() {
        assert_eq!(
            code(verified(MANIFEST.as_bytes(), SIGNATURE, &TRUSTED)),
            "update.unsigned"
        );
    }

    #[test]
    fn changed_bytes_are_refused() {
        let changed = MANIFEST.replace("1.2.3", "1.2.4");
        assert_eq!(
            code(verified(changed.as_bytes(), SIGNATURE, &[TEST_KEY])),
            "update.unsigned"
        );
        let longer = format!("{MANIFEST} ");
        assert_eq!(
            code(verified(longer.as_bytes(), SIGNATURE, &[TEST_KEY])),
            "update.unsigned"
        );
    }

    #[test]
    fn a_legacy_or_garbled_signature_is_refused() {
        let garbled = SIGNATURE.replace("BJKm", "BJKn");
        for signature in [LEGACY, garbled.as_str(), "", "untrusted comment: x\n"] {
            let result = verified(MANIFEST.as_bytes(), signature, &[TEST_KEY]);
            assert_eq!(code(result), "update.unsigned", "{signature:?}");
        }
    }

    #[test]
    fn without_keys_nothing_is_trusted() {
        assert_eq!(
            code(verified(MANIFEST.as_bytes(), SIGNATURE, &[])),
            "update.unsigned"
        );
        let broken = ["not a key"];
        assert_eq!(
            code(verified(MANIFEST.as_bytes(), SIGNATURE, &broken)),
            "update.unsigned"
        );
        verified(MANIFEST.as_bytes(), SIGNATURE, &["not a key", TEST_KEY]).unwrap();
    }

    #[test]
    fn signed_bytes_that_are_no_manifest_are_refused() {
        let result = verified(OTHER.as_bytes(), OTHER_SIGNATURE, &[TEST_KEY]);
        assert_eq!(code(result), "update.bad_manifest");
    }

    /// The key id minisign shows: bytes 2 to 10 of the key, read backwards.
    fn key_id(key: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let sixes: Vec<u32> = key
            .bytes()
            .map(|byte| ALPHABET.iter().position(|&letter| letter == byte).unwrap())
            .map(|six| u32::try_from(six).unwrap())
            .collect();
        let bytes: Vec<u8> = sixes
            .chunks(4)
            .flat_map(|group| {
                let value = group.iter().fold(0, |acc, six| (acc << 6) | six);
                value.to_be_bytes().into_iter().skip(1)
            })
            .collect();
        let id: [u8; 8] = bytes[2..10].try_into().unwrap();
        format!("{:016X}", u64::from_le_bytes(id))
    }

    #[test]
    fn every_build_trusts_the_release_key_and_its_backup() {
        for key in TRUSTED {
            PublicKey::from_base64(key).unwrap();
        }
        assert_eq!(
            TRUSTED.map(key_id),
            ["4D9033C9C7C1F681", "FC5980A8D9FB8637"]
        );
        assert_eq!(key_id(TEST_KEY), "377D1E2DAE6ACDC6");
    }
}
