/// A file or folder name for `id`, a token id, after `prefix`. A capital becomes `.` and its
/// small letter, so ids differing only in case don't share a name; the prefix keeps Windows'
/// device names, such as `CON`, out.
pub(crate) fn case_safe_name(prefix: &str, id: &str) -> String {
    let mut name = String::from(prefix);
    for letter in id.chars() {
        if letter.is_ascii_uppercase() {
            name.push('.');
        }
        name.push(letter.to_ascii_lowercase());
    }
    name
}

/// The id that `name` is the [`case_safe_name`] of after `prefix`, if it is one.
pub(crate) fn case_safe_id(prefix: &str, name: &str) -> Option<String> {
    let mut id = String::new();
    let mut letters = name.strip_prefix(prefix)?.chars();
    while let Some(letter) = letters.next() {
        id.push(if letter == '.' {
            letters.next()?.to_ascii_uppercase()
        } else {
            letter
        });
    }
    (case_safe_name(prefix, &id) == name).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_differing_in_case_get_names_differing_in_more_than_case() {
        assert_eq!(case_safe_name("w-", "aB-c"), "w-a.b-c");
        assert_ne!(
            case_safe_name("w-", "ab").to_lowercase(),
            case_safe_name("w-", "Ab").to_lowercase()
        );
    }

    #[test]
    fn names_turn_back_into_their_ids_and_other_names_into_none() {
        for back in ["ab", "Ab", "aB", "AB", "a-b", "CON", "con"] {
            assert_eq!(
                case_safe_id("w-", &case_safe_name("w-", back)).as_deref(),
                Some(back)
            );
        }
        for other in ["ab", "w-a.", "w-a..b", "w-.1", "w-A", "x-ab"] {
            assert_eq!(case_safe_id("w-", other), None, "{other}");
        }
    }
}
