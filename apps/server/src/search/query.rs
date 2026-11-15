/// The most bytes a search's text may hold.
pub(crate) const MAX_TEXT: usize = 256;
/// The most words a search's text may hold.
const MAX_WORDS: usize = 8;
/// A trigram index finds only words of at least this many characters.
const MIN_WORD: usize = 3;
/// The most characters a word may hold, so that its whole match fits in a snippet.
pub(crate) const MAX_WORD: usize = 64;

/// A search's words, ready for the index. Each long word is quoted as an FTS5 phrase, so that
/// nothing in it is read as query syntax; each short one is an escaped `LIKE` pattern.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Query {
    /// What a row must match, and its title too to come first: every long word.
    pub(crate) words: String,
    /// What a row's text must also hold, as patterns for `LIKE … ESCAPE '\'`.
    pub(crate) short: Vec<String>,
    /// Every word, folded as [`fold`] folds, for a snippet to mark.
    pub(crate) marked: Vec<String>,
}

/// Text past [`MAX_TEXT`] bytes or [`MAX_WORDS`] words, or with a word past [`MAX_WORD`]
/// characters.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TooLong;

/// The query for `text`, or none when no word in it is long enough to look up.
pub(crate) fn parse(text: &str) -> Result<Option<Query>, TooLong> {
    // FTS5 reads its query up to a NUL, so a control character separates words too.
    let words: Vec<&str> = text
        .split(|ch: char| ch.is_whitespace() || ch.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let too_long = |word: &&str| word.chars().count() > MAX_WORD;
    if text.len() > MAX_TEXT || words.len() > MAX_WORDS || words.iter().any(too_long) {
        return Err(TooLong);
    }
    let marked = words
        .iter()
        .map(|word| word.chars().map(fold).collect())
        .collect();
    let (long, short): (Vec<&str>, Vec<&str>) = words
        .into_iter()
        .partition(|word| word.chars().count() >= MIN_WORD);
    if long.is_empty() {
        return Ok(None);
    }
    let words = long
        .iter()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some(Query {
        words,
        short: short.into_iter().map(pattern).collect(),
        marked,
    }))
}

/// `ch` as a snippet compares it: lower case, and a Latin letter without its accent, much as
/// the index's `remove_diacritics` reads it. One character always folds to one.
pub(crate) fn fold(ch: char) -> char {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    match lower {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'è'..='ë' | 'ē' | 'ę' | 'ě' => 'e',
        'ì'..='ï' | 'ī' => 'i',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ő' => 'o',
        'ù'..='ü' | 'ū' | 'ů' | 'ű' => 'u',
        'ý' | 'ÿ' => 'y',
        'š' | 'ś' => 's',
        'ž' | 'ź' | 'ż' => 'z',
        other => other,
    }
}

/// A `LIKE` pattern for text that holds `word` anywhere.
fn pattern(word: &str) -> String {
    let escaped = word
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_");
    format!("%{escaped}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> Query {
        parse(text).unwrap().unwrap()
    }

    #[test]
    fn each_long_word_is_one_quoted_phrase_whatever_it_holds() {
        let parsed = query("  release \"notes\" OR NEAR(x*  ");

        assert_eq!(parsed.words, r#""release" """notes""" "NEAR(x*""#);
        assert_eq!(parsed.short, ["%OR%"]);
    }

    #[test]
    fn words_count_characters_not_bytes() {
        assert_eq!(query("café").words, "\"café\"");
        assert_eq!(query("ab éé abc").short, ["%ab%", "%éé%"]);
        assert_eq!(parse("éé"), Ok(None));
    }

    #[test]
    fn control_characters_part_words_and_never_reach_a_phrase() {
        let parsed = query("rel\0ease\u{1}notes\tok");

        assert_eq!(parsed.words, r#""rel" "ease" "notes""#);
        assert_eq!(parsed.short, ["%ok%"]);
    }

    #[test]
    fn short_words_match_only_as_themselves() {
        assert_eq!(
            query(r"plan 5% a_ \x").short,
            [r"%5\%%", r"%a\_%", r"%\\x%"]
        );
    }

    #[test]
    fn text_without_a_long_word_looks_nothing_up() {
        for text in ["", "   ", "a b", "is it"] {
            assert_eq!(parse(text), Ok(None), "{text:?}");
        }
    }

    #[test]
    fn text_is_bounded_in_bytes_and_words() {
        // Four words of 63 or 64 bytes and three spaces: 256 bytes.
        let longest = format!(
            "{} {}",
            vec!["a".repeat(MAX_WORD - 1); 3].join(" "),
            "a".repeat(MAX_WORD)
        );
        let words = ["word"; MAX_WORDS];

        assert_eq!(longest.len(), MAX_TEXT);
        parse(&longest).unwrap().unwrap();
        assert_eq!(parse(&format!("{longest} b")), Err(TooLong));
        parse(&words.join(" ")).unwrap().unwrap();
        parse(&"é".repeat(MAX_WORD)).unwrap().unwrap();
        assert_eq!(parse(&"é".repeat(MAX_WORD + 1)), Err(TooLong));
        assert_eq!(parse(&format!("{} more", words.join(" "))), Err(TooLong));
    }

    #[test]
    fn a_letter_folds_to_its_lower_case_without_its_accent() {
        let folded: String = "ÀÉÎÕÜçñýšžĘŁ1".chars().map(fold).collect();

        assert_eq!(folded, "aeioucnysze\u{142}1");
        assert_eq!(query("Café ab").marked, ["cafe", "ab"]);
    }
}
