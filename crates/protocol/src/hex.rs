//! Lowercase hex, the form keys, digests and sealed bytes travel in.

/// `bytes` as lowercase hex.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    let nibbles = bytes.iter().flat_map(|byte| [byte >> 4, byte & 0xf]);
    nibbles
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// The bytes `hex` spells, unless it isn't pairs of lowercase hex digits.
pub(crate) fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let nibble = |digit: u8| match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    };
    hex.as_bytes()
        .chunks(2)
        .map(|pair| match pair {
            [high, low] => Some(nibble(*high)? * 16 + nibble(*low)?),
            _ => None,
        })
        .collect()
}

pub(crate) fn is_lower_hex(text: &str) -> bool {
    text.bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip_through_lowercase_hex() {
        let bytes = [0x00, 0x09, 0x0a, 0x7f, 0xa5, 0xff];

        assert_eq!(to_hex(&bytes), "00090a7fa5ff");
        assert_eq!(from_hex("00090a7fa5ff"), Some(bytes.to_vec()));
        assert_eq!(from_hex(""), Some(Vec::new()));
        assert_eq!(to_hex(&[]), "");
    }

    #[test]
    fn only_pairs_of_lowercase_digits_are_hex() {
        for bad in ["0", "abc", "0A", "0g", "g0", "/0", ":0", "`0", " 0"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
        assert!(is_lower_hex("0123456789abcdef"));
        assert!(is_lower_hex(""));
        for bad in ["A", "g", "/", ":", "`", " ", "é"] {
            assert!(!is_lower_hex(bad), "{bad}");
        }
    }
}
