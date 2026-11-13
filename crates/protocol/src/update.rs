use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::token::token_id;

token_id!(
    /// A Rust target a release is built for, such as `x86_64-unknown-linux-musl`.
    Target,
    "not a build target"
);

/// A release, as its signed manifest describes it; a core reads it to update itself. It is
/// published with the release's builds, and its signature beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    pub version: ReleaseVersion,
    pub commit: Commit,
    /// The protocol the release speaks; a core asks its owner before installing one that differs.
    pub protocol: u32,
    pub builds: Vec<Build>,
}

/// One target's binary in a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Build {
    pub target: Target,
    pub url: DownloadUrl,
    pub size: u64,
    pub sha256: Sha256,
}

impl Manifest {
    #[must_use]
    pub fn build_for(&self, target: &str) -> Option<&Build> {
        self.builds
            .iter()
            .find(|build| build.target.as_str() == target)
    }
}

/// A release's `major.minor.patch`, ordered by its numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

// The schema's form of `ReleaseVersion::from_str`; a test keeps the two in step.
const VERSION_PATTERN: &str = r"^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$";

impl FromStr for ReleaseVersion {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const WHAT: &str = "not a major.minor.patch version";
        let number = |part: &str| {
            let digits = (1..=9).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
            let leading_zero = part.len() > 1 && part.starts_with('0');
            if digits && !leading_zero {
                part.parse().map_err(|_| WHAT)
            } else {
                Err(WHAT)
            }
        };
        let mut parts = text.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(WHAT);
        };
        Ok(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
        })
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for ReleaseVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ReleaseVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for ReleaseVersion {
    fn schema_name() -> Cow<'static, str> {
        "ReleaseVersion".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "pattern": VERSION_PATTERN })
    }
}

/// Declares a string of lowercase hex digits of a fixed length.
macro_rules! hex_string {
    ($(#[$doc:meta])* $name:ident, $len:literal, $pattern:literal, $what:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
        pub struct $name(#[schemars(regex(pattern = $pattern))] String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = &'static str;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
                if text.len() == $len && text.bytes().all(hex) {
                    Ok(Self(text.to_owned()))
                } else {
                    Err($what)
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
            }
        }
    };
}

hex_string!(
    /// A full git commit id: 40 lowercase hex digits.
    Commit,
    40,
    "^[0-9a-f]{40}$",
    "not a full commit id"
);

hex_string!(
    /// A SHA-256 digest: 64 lowercase hex digits.
    Sha256,
    64,
    "^[0-9a-f]{64}$",
    "not a SHA-256 digest"
);

/// An `https://` address with no spaces or control characters, at most 2,048 bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct DownloadUrl(#[schemars(regex(pattern = URL_PATTERN), length(max = URL_MAX))] String);

const URL_MAX: usize = 2048;

// The schema's form of `DownloadUrl::from_str`; a test keeps the two in step.
const URL_PATTERN: &str = r"^https://[!-~]+$";

impl DownloadUrl {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for DownloadUrl {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let rest = text.strip_prefix("https://").unwrap_or_default();
        if text.len() <= URL_MAX && !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_graphic()) {
            Ok(Self(text.to_owned()))
        } else {
            Err("not an https address")
        }
    }
}

impl<'de> Deserialize<'de> for DownloadUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use regex::Regex;
    use serde_json::json;

    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest() -> serde_json::Value {
        json!({
            "version": "1.2.3",
            "commit": COMMIT,
            "protocol": 1,
            "builds": [{
                "target": "x86_64-unknown-linux-musl",
                "url": "https://example.com/weacon-x86_64-unknown-linux-musl",
                "size": 7_148_880,
                "sha256": SHA,
            }],
        })
    }

    #[test]
    fn a_manifest_round_trips_and_finds_its_target() {
        let parsed: Manifest = serde_json::from_value(manifest()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), manifest());
        let build = parsed.build_for("x86_64-unknown-linux-musl").unwrap();
        assert_eq!(build.size, 7_148_880);
        assert_eq!(build.sha256.as_str(), SHA);
        assert_eq!(parsed.commit.as_str(), COMMIT);
        assert!(parsed.build_for("aarch64-apple-darwin").is_none());
    }

    #[test]
    fn newer_fields_are_ignored() {
        let mut newer = manifest();
        newer["notes"] = json!("later");
        newer["builds"][0]["signed_by"] = json!("k2");
        let parsed: Manifest = serde_json::from_value(newer).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), manifest());
    }

    #[test]
    fn a_bad_field_refuses_the_whole_manifest() {
        let cases = [
            ("/version", json!("1.2")),
            ("/commit", json!(COMMIT.to_uppercase())),
            ("/builds/0/sha256", json!(&SHA[1..])),
            ("/builds/0/url", json!("http://example.com/weacon")),
            ("/builds/0/target", json!("x86 64")),
            ("/builds/0/size", json!(-1)),
        ];
        for (pointer, value) in cases {
            let mut bad = manifest();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                serde_json::from_value::<Manifest>(bad).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn versions_order_by_their_numbers() {
        let version = |text: &str| text.parse::<ReleaseVersion>().unwrap();
        assert!(version("0.10.0") > version("0.9.9"));
        assert!(version("1.0.0") > version("0.99.99"));
        assert!(version("1.2.10") > version("1.2.9"));
        assert_eq!(version("1.2.3").to_string(), "1.2.3");
        assert_eq!(
            version("4.5.6"),
            ReleaseVersion {
                major: 4,
                minor: 5,
                patch: 6
            }
        );
    }

    #[test]
    fn the_patterns_match_the_parsers() {
        let checks: [(&str, fn(&str) -> bool, &[&str]); 4] = [
            (
                VERSION_PATTERN,
                |t| t.parse::<ReleaseVersion>().is_ok(),
                &[
                    "0.0.0",
                    "1.2.3",
                    "999999999.0.1",
                    "1.2",
                    "1.2.3.4",
                    "01.2.3",
                    "1.2.-3",
                    "1.2.3-rc1",
                    "1000000000.0.0",
                    "",
                    "a.b.c",
                    "1..3",
                ],
            ),
            (
                URL_PATTERN,
                |t| t.parse::<DownloadUrl>().is_ok(),
                &[
                    "https://a",
                    "https://example.com/x?y=1",
                    "https://",
                    "http://a",
                    "https://a b",
                    "https://a\n",
                    "https://é",
                    " https://a",
                ],
            ),
            (
                "^[0-9a-f]{40}$",
                |t| t.parse::<Commit>().is_ok(),
                &[COMMIT, "abc", "g0123456789abcdef0123456789abcdef0123456"],
            ),
            (
                "^[0-9a-f]{64}$",
                |t| t.parse::<Sha256>().is_ok(),
                &[SHA, "", COMMIT],
            ),
        ];
        for (pattern, parses, samples) in checks {
            let regex = Regex::new(pattern).unwrap();
            for sample in samples {
                assert_eq!(
                    regex.is_match(sample),
                    parses(sample),
                    "{pattern} {sample:?}"
                );
            }
        }
    }

    #[test]
    fn urls_are_bounded() {
        let long = format!("https://{}", "a".repeat(URL_MAX - 8));
        assert!(long.parse::<DownloadUrl>().is_ok());
        assert!(format!("{long}a").parse::<DownloadUrl>().is_err());
    }
}
