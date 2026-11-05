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

#[cfg(test)]
mod tests {
    use super::*;

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
