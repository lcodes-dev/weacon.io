//! The bytes of a pick's screenshot, which travels as base64.

/// `text`, standard base64 with its padding, decoded; `None` when it isn't that.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let last = bytes.len() / 4;
    for (at, group) in bytes.chunks_exact(4).enumerate() {
        let pad = group.iter().rev().take_while(|byte| **byte == b'=').count();
        if pad > 2 || (pad > 0 && at + 1 != last) {
            return None;
        }
        let mut word = 0u32;
        for byte in group.iter().take(4 - pad) {
            word = (word << 6) | u32::from(value(*byte)?);
        }
        word <<= 6 * u32::try_from(pad).ok()?;
        let [_, high, middle, low] = word.to_be_bytes();
        out.extend([high, middle, low].iter().take(3 - pad));
    }
    Some(out)
}

fn value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_base64_decodes_with_its_padding() {
        let cases: [(&str, &[u8]); 6] = [
            ("", b""),
            ("Zg==", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("iVBORw0KGgo=", b"\x89PNG\r\n\x1a\n"),
            ("+/+/", b"\xfb\xff\xbf"),
        ];
        for (text, bytes) in cases {
            assert_eq!(decode(text).as_deref(), Some(bytes), "{text}");
        }
        let all: String = ('A'..='Z').chain('a'..='z').chain('0'..='9').collect();
        assert_eq!(decode(&format!("{all}+/")).map(|out| out.len()), Some(48));
    }

    #[test]
    fn anything_else_is_refused() {
        for text in [
            "Zg=", "Zg===", "Z===", "Zg==Zm9v", "Zm9v!A==", "Zm9-", "Zm 9",
        ] {
            assert_eq!(decode(text), None, "{text}");
        }
    }
}
