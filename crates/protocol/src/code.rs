use std::borrow::Cow;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

/// An error code such as `store.full`: two or more lowercase words joined by dots.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct Code(#[schemars(regex(pattern = PATTERN), length(max = MAX_LEN))] Cow<'static, str>);

// Codes come from peers too, and end up in log lines and error reports.
const MAX_LEN: usize = 64;

// The schema's form of `is_code`; a test keeps the two in step.
const PATTERN: &str = r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$";

/// A [`Code`], checked at compile time: `code!("store.full")`.
///
/// ```compile_fail
/// let code = weacon_protocol::code!("Store.full");
/// ```
#[macro_export]
macro_rules! code {
    ($code:literal) => {
        const { $crate::Code::new($code) }
    };
}

impl Code {
    /// Use [`code!`], which checks the code at compile time.
    ///
    /// # Panics
    /// Unless `code` is dotted lowercase words.
    #[doc(hidden)]
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        assert!(
            is_code(code),
            "error codes are dotted lowercase words, like store.full"
        );
        Self(Cow::Borrowed(code))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        if is_code(&code) {
            Ok(Self(Cow::Owned(code)))
        } else {
            Err(de::Error::custom("not a dotted lowercase error code"))
        }
    }
}

const fn is_code(code: &str) -> bool {
    if code.len() > MAX_LEN {
        return false;
    }
    let mut rest = code.as_bytes();
    let mut words = 1;
    let mut word_start = true;
    while let [byte, tail @ ..] = rest {
        let byte = *byte;
        if word_start {
            if !byte.is_ascii_lowercase() {
                return false;
            }
            word_start = false;
        } else if byte == b'.' {
            words += 1;
            word_start = true;
        } else if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
            return false;
        }
        rest = tail;
    }
    !word_start && words > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dotted_lowercase_codes_only() {
        let pattern = regex::Regex::new(PATTERN).unwrap();
        for good in [
            "store.full",
            "driver.timeout",
            "ssh.host_key_changed",
            "agent_sdk.timeout",
            "a1.b2.c_3",
        ] {
            assert!(is_code(good), "{good}");
            assert!(pattern.is_match(good), "{good}");
        }
        for bad in [
            "",
            "store",
            "Store.full",
            "store.",
            ".full",
            "store..full",
            "_store.full",
            "store.Full",
            "store.2x",
            "störe.full",
            "store full",
        ] {
            assert!(!is_code(bad), "{bad}");
            assert!(!pattern.is_match(bad), "{bad}");
        }
    }

    #[test]
    fn travels_as_a_string_and_is_checked_on_arrival() {
        let code = crate::code!("store.full");

        assert_eq!(serde_json::to_string(&code).unwrap(), r#""store.full""#);
        assert_eq!(
            serde_json::from_str::<Code>(r#""store.full""#).unwrap(),
            code
        );
        assert_eq!(code.as_str(), "store.full");
        serde_json::from_str::<Code>(r#""Store full""#).unwrap_err();
    }

    #[test]
    fn codes_are_at_most_64_bytes() {
        let longest = format!("a.{}", "b".repeat(62));

        assert!(is_code(&longest));
        assert!(!is_code(&format!("{longest}c")));
    }
}
