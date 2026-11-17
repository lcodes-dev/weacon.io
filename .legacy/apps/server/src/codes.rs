//! The short codes a person reads in one place and types in another: sign-in and pairing.

use weacon_observability::Fault;

use crate::random::random_u64;

/// Crockford's base 32: no I, L, O or U, which read like other letters.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A new code of eight letters and digits: 40 random bits.
pub(crate) fn new_code() -> Result<String, Fault> {
    Ok(code_text(random_u64()?))
}

fn code_text(word: u64) -> String {
    (0..8)
        .filter_map(|place| {
            let index = usize::try_from((word >> (5 * place)) & 31).ok()?;
            ALPHABET.get(index).copied().map(char::from)
        })
        .collect()
}

/// A code as its person reads it: `ABCD-EFGH`.
pub(crate) fn shown(code: &str) -> String {
    let (first, second) = code.split_at_checked(4).unwrap_or((code, ""));
    format!("{first}-{second}")
}

/// A code as a person typed it, read as its letters: any case, with or without spaces and
/// dashes, and the letters that look like digits taken for them.
pub(crate) fn normalised(code: &str) -> String {
    code.chars()
        .filter(|letter| !letter.is_whitespace() && *letter != '-')
        .map(|letter| match letter.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_eight_letters_read_as_typed() {
        assert_eq!(code_text(0), "00000000");
        assert_eq!(code_text(u64::MAX), "ZZZZZZZZ");
        assert_eq!(code_text(u64::from_le_bytes([1; 8])), "1802G040");
        assert_eq!(new_code().unwrap().len(), 8);
        assert_eq!(shown("ABCDEFGH"), "ABCD-EFGH");
        assert_eq!(normalised(" abcd-efgh "), "ABCDEFGH");
        assert_eq!(normalised("oO iI lL"), "001111");
        assert_eq!(normalised("1234 5678"), "12345678");
    }
}
