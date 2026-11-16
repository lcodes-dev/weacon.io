use schemars::JsonSchema;
use serde::Serialize;

use crate::hex::hex_string;
use crate::token::checked_string;

hex_string!(
    /// A SHA-256 digest: 64 lowercase hex digits. Not the `sha2::Sha256` hasher.
    Sha256,
    64,
    "not a SHA-256 digest"
);

/// An `https://` address of printable ASCII, at most 2,048 bytes, with a plain host name and
/// optionally a port: no user name or password.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct DownloadUrl(#[schemars(regex(pattern = URL_PATTERN), length(max = URL_MAX))] String);

const URL_MAX: usize = 2048;

// The schema's form of `is_https_url`; a test keeps the two in step.
const URL_PATTERN: &str = r"^https://[A-Za-z0-9.-]+(:[0-9]{1,5})?([/?#][!-~]*)?$";

checked_string!(DownloadUrl, is_https_url, "not an https address");

fn is_https_url(text: &str) -> bool {
    text.len() <= URL_MAX
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
        })
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;
    use crate::MachineKey;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn matches_its_parser(pattern: &str, parses: impl Fn(&str) -> bool, samples: &[&str]) {
        let regex = Regex::new(pattern).unwrap();
        for sample in samples {
            assert_eq!(regex.is_match(sample), parses(sample), "{sample:?}");
        }
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
            "0123456789abcdef0123456789abcdef01234567",
            SHA,
            "",
            "abc",
            "g123456789abcdef0123456789abcdef01234567",
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
        ];
        let sha = |text: &str| Sha256::try_from(text.to_owned()).is_ok();
        let key = |text: &str| MachineKey::try_from(text.to_owned()).is_ok();
        matches_its_parser(Sha256::PATTERN, sha, &samples);
        matches_its_parser(MachineKey::PATTERN, key, &samples);
    }

    #[test]
    fn a_digest_is_written_in_lowercase_hex() {
        let bytes = std::array::from_fn(|i| u8::try_from(i * 8 + 7).unwrap());
        let hex = "070f171f272f373f474f575f676f777f878f979fa7afb7bfc7cfd7dfe7eff7ff";

        assert_eq!(Sha256::from_bytes(bytes).as_str(), hex);
    }

    #[test]
    fn urls_are_bounded() {
        let long = format!("https://{}", "a".repeat(URL_MAX - 8));
        DownloadUrl::try_from(long.clone()).unwrap();
        DownloadUrl::try_from(format!("{long}a")).unwrap_err();
    }
}
