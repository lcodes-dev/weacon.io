//! The rule for ids made of plain tokens, such as request and run ids.

pub(crate) const TOKEN_MAX: usize = 64;

// The schema's form of `is_token`; a test keeps the two in step.
pub(crate) const TOKEN_PATTERN: &str = r"^[A-Za-z0-9_-]+$";

/// 1–64 ASCII letters, digits, `-` or `_`.
pub(crate) fn is_token(id: &str) -> bool {
    (1..=TOKEN_MAX).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Declares an id that is a token; parsing anything else fails with the message `$what`.
macro_rules! token_id {
    ($(#[$doc:meta])* $name:ident, $what:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, schemars::JsonSchema)]
        pub struct $name(
            #[schemars(
                regex(pattern = $crate::token::TOKEN_PATTERN),
                length(max = $crate::token::TOKEN_MAX)
            )]
            String,
        );

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(id: String) -> Result<Self, Self::Error> {
                if $crate::token::is_token(&id) {
                    Ok(Self(id))
                } else {
                    Err(id)
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = &'static str;

            fn from_str(id: &str) -> Result<Self, Self::Err> {
                Self::try_from(id.to_owned()).map_err(|_| $what)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::try_from(<String as serde::Deserialize>::deserialize(deserializer)?)
                    .map_err(|_| <D::Error as serde::de::Error>::custom($what))
            }
        }
    };
}

pub(crate) use token_id;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    // Every id comes from `token_id!`, so one of them stands for all.
    use crate::RunId;

    #[test]
    fn an_id_takes_only_tokens_however_it_arrives() {
        let id: RunId = "t_1".parse().unwrap();

        assert_eq!(id.as_str(), "t_1");
        assert_eq!(RunId::try_from("t_1".to_owned()), Ok(id.clone()));
        assert_eq!(serde_json::to_value(&id).unwrap(), json!("t_1"));
        assert_eq!(serde_json::from_value::<RunId>(json!("t_1")).unwrap(), id);
        assert_eq!(RunId::try_from("a b".to_owned()), Err("a b".to_owned()));
        assert_eq!("a b".parse::<RunId>(), Err("not a run id"));
        let bad = serde_json::from_value::<RunId>(json!("../x")).unwrap_err();
        assert!(bad.to_string().contains("not a run id"), "{bad}");
    }

    #[test]
    fn tokens_are_short_and_plain_and_match_the_schema() {
        let pattern = regex::Regex::new(TOKEN_PATTERN).unwrap();
        let longest = "a".repeat(TOKEN_MAX);
        for good in ["01JC2YQ7Z8", "9b1d-4c2e_f", longest.as_str()] {
            assert!(is_token(good), "{good}");
            assert!(pattern.is_match(good), "{good}");
        }
        let too_long = "a".repeat(TOKEN_MAX + 1);
        for bad in ["", "a b", "a/b", "é", too_long.as_str()] {
            assert!(!is_token(bad), "{bad}");
            // The schema states the length limit apart from the pattern.
            assert!(bad.len() > TOKEN_MAX || !pattern.is_match(bad), "{bad}");
        }
    }
}
