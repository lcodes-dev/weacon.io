use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::token::token_id;

token_id!(
    /// A Rust target a release is built for, such as `x86_64-unknown-linux-musl`.
    BuildTarget,
    "not a build target"
);

/// A release, as its signed manifest describes it; a core reads it to update itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseManifest {
    pub version: ReleaseVersion,
    pub commit: Commit,
    /// When the release was signed, in seconds since the Unix epoch, so a core can tell a
    /// manifest that is being held back from the newest one.
    pub published_at: u64,
    pub protocol: u32,
    /// At most one for each target.
    #[serde(deserialize_with = "one_per_target")]
    pub builds: Vec<ReleaseBuild>,
}

/// One target's binary in a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseBuild {
    pub target: BuildTarget,
    pub url: DownloadUrl,
    pub size: u64,
    pub sha256: Sha256,
}

impl ReleaseManifest {
    #[must_use]
    pub fn build_for(&self, target: &BuildTarget) -> Option<&ReleaseBuild> {
        self.builds.iter().find(|build| &build.target == target)
    }
}

fn one_per_target<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ReleaseBuild>, D::Error> {
    let builds = Vec::<ReleaseBuild>::deserialize(deserializer)?;
    let mut targets: Vec<_> = builds.iter().map(|build| build.target.as_str()).collect();
    targets.sort_unstable();
    targets.dedup();
    if targets.len() == builds.len() {
        Ok(builds)
    } else {
        Err(de::Error::custom("two builds for one target"))
    }
}

/// A release's `major.minor.patch`, ordered by its numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

// The schema's form of `ReleaseVersion::from_str`; a test keeps the two in step.
const VERSION_PATTERN: &str = r"^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$";

impl FromStr for ReleaseVersion {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const WHAT: &str = "not a major.minor.patch version";
        let number = |part: &str| {
            let digits =
                (1..=9).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit());
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
        json_schema!({
            "description": "A release's `major.minor.patch`, ordered by its numbers.",
            "type": "string",
            "pattern": VERSION_PATTERN,
        })
    }
}

/// Declares a string of `$len` lowercase hex digits; parsing anything else fails with `$what`.
macro_rules! hex_string {
    ($(#[$doc:meta])* $name:ident, $len:literal, $what:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
        pub struct $name(#[schemars(regex(pattern = Self::PATTERN))] String);

        impl $name {
            // The schema's form of `try_from`; a test keeps the two in step.
            const PATTERN: &str = concat!("^[0-9a-f]{", stringify!($len), "}$");

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(text: String) -> Result<Self, Self::Error> {
                if text.len() == $len && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
                    Ok(Self(text))
                } else {
                    Err(text)
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::try_from(String::deserialize(deserializer)?)
                    .map_err(|_| de::Error::custom($what))
            }
        }
    };
}

hex_string!(
    /// A full git commit id: 40 lowercase hex digits.
    Commit,
    40,
    "not a full commit id"
);

hex_string!(
    /// A SHA-256 digest: 64 lowercase hex digits.
    Sha256,
    64,
    "not a SHA-256 digest"
);

impl From<[u8; 32]> for Sha256 {
    fn from(digest: [u8; 32]) -> Self {
        Self(crate::to_hex(&digest))
    }
}

/// An `https://` address of printable ASCII, at most 2,048 bytes, with a plain host name and
/// optionally a port: no user name or password.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct DownloadUrl(#[schemars(regex(pattern = URL_PATTERN), length(max = URL_MAX))] String);

const URL_MAX: usize = 2048;

// The schema's form of `DownloadUrl::try_from`; a test keeps the two in step.
const URL_PATTERN: &str = r"^https://[A-Za-z0-9.-]+(:[0-9]{1,5})?([/?#][!-~]*)?$";

impl DownloadUrl {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DownloadUrl {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let valid = text.len() <= URL_MAX
            && text.strip_prefix("https://").is_some_and(|rest| {
                let (authority, path) = rest
                    .find(['/', '?', '#'])
                    .map_or((rest, ""), |at| rest.split_at(at));
                let (host, port) = authority.split_once(':').unwrap_or((authority, "0"));
                let host_ok = !host.is_empty()
                    && host
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte));
                let port_ok =
                    (1..=5).contains(&port.len()) && port.bytes().all(|byte| byte.is_ascii_digit());
                host_ok && port_ok && path.bytes().all(|byte| byte.is_ascii_graphic())
            });
        if valid { Ok(Self(text)) } else { Err(text) }
    }
}

impl<'de> Deserialize<'de> for DownloadUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not an https address"))
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
            "published_at": 1_794_614_400,
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
        let parsed: ReleaseManifest = serde_json::from_value(manifest()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), manifest());
        let target = |name: &str| BuildTarget::try_from(name.to_owned()).unwrap();
        let build = parsed
            .build_for(&target("x86_64-unknown-linux-musl"))
            .unwrap();
        assert_eq!(build.size, 7_148_880);
        assert_eq!(
            build.url.as_str(),
            "https://example.com/weacon-x86_64-unknown-linux-musl"
        );
        assert_eq!(parsed.published_at, 1_794_614_400);
        assert_eq!(build.sha256.as_str(), SHA);
        assert_eq!(parsed.commit.as_str(), COMMIT);
        assert!(parsed.build_for(&target("aarch64-apple-darwin")).is_none());
    }

    #[test]
    fn two_builds_for_one_target_refuse_the_manifest() {
        let mut twice = manifest();
        let mut second = twice["builds"][0].clone();
        second["size"] = json!(1);
        twice["builds"].as_array_mut().unwrap().push(second.clone());
        serde_json::from_value::<ReleaseManifest>(twice.clone()).unwrap_err();
        second["target"] = json!("aarch64-apple-darwin");
        *twice["builds"].get_mut(1).unwrap() = second;
        let parsed: ReleaseManifest = serde_json::from_value(twice).unwrap();
        assert_eq!(parsed.builds.len(), 2);
    }

    #[test]
    fn newer_fields_are_ignored() {
        let mut newer = manifest();
        newer["notes"] = json!("later");
        newer["builds"][0]["signed_by"] = json!("k2");
        let parsed: ReleaseManifest = serde_json::from_value(newer).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), manifest());
    }

    #[test]
    fn a_bad_field_refuses_the_whole_manifest() {
        let cases = [
            ("/version", json!("1.2")),
            ("/commit", json!(COMMIT.to_uppercase())),
            ("/builds/0/sha256", json!(COMMIT)),
            ("/builds/0/url", json!("http://example.com/weacon")),
            ("/builds/0/target", json!("x86 64")),
            ("/builds/0/size", json!(-1)),
            ("/published_at", json!("today")),
        ];
        for (pointer, value) in cases {
            let mut bad = manifest();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                serde_json::from_value::<ReleaseManifest>(bad).is_err(),
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

    fn matches_its_parser(pattern: &str, parses: impl Fn(&str) -> bool, samples: &[&str]) {
        let regex = Regex::new(pattern).unwrap();
        for sample in samples {
            assert_eq!(regex.is_match(sample), parses(sample), "{sample:?}");
        }
    }

    #[test]
    fn the_version_pattern_matches_its_parser() {
        let samples = [
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
        ];
        matches_its_parser(
            VERSION_PATTERN,
            |text| text.parse::<ReleaseVersion>().is_ok(),
            &samples,
        );
    }

    #[test]
    fn the_url_pattern_matches_its_parser() {
        let samples = [
            "https://a",
            "https://example.com/x?y=1",
            "https://",
            "http://a",
            "https://a b",
            "https://a\n",
            "https://é",
            " https://a",
            "https://#",
            "https:///x",
            "https://user:pw@example.com/x",
            "https://user@example.com",
            "https://example.com:443/x",
            "https://example.com:/x",
            "https://example.com:123456",
            "https://example.com#part",
            "https://a:1",
            "https://a:b",
        ];
        let parses = |text: &str| DownloadUrl::try_from(text.to_owned()).is_ok();
        matches_its_parser(URL_PATTERN, parses, &samples);
    }

    #[test]
    fn the_hex_patterns_match_their_parsers() {
        let samples = [
            COMMIT,
            SHA,
            "",
            "abc",
            "g123456789abcdef0123456789abcdef01234567",
        ];
        let commit = |text: &str| Commit::try_from(text.to_owned()).is_ok();
        let sha = |text: &str| Sha256::try_from(text.to_owned()).is_ok();
        matches_its_parser(Commit::PATTERN, commit, &samples);
        matches_its_parser(Sha256::PATTERN, sha, &samples);
    }

    #[test]
    fn a_digest_is_written_in_lowercase_hex() {
        let bytes = std::array::from_fn(|i| u8::try_from(i * 8 + 7).unwrap());
        let hex = "070f171f272f373f474f575f676f777f878f979fa7afb7bfc7cfd7dfe7eff7ff";

        assert_eq!(Sha256::from(bytes).as_str(), hex);
    }

    #[test]
    fn urls_are_bounded() {
        let long = format!("https://{}", "a".repeat(URL_MAX - 8));
        DownloadUrl::try_from(long.clone()).unwrap();
        DownloadUrl::try_from(format!("{long}a")).unwrap_err();
    }
}
