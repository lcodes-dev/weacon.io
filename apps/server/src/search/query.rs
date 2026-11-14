/// The most bytes a search's text may hold.
pub(crate) const MAX_TEXT: usize = 256;
/// The most words a search's text may hold.
const MAX_WORDS: usize = 8;
/// A trigram index finds only words of at least this many characters.
const MIN_WORD: usize = 3;

/// A search's words, ready for the index. Each long word is quoted as an FTS5 phrase, so that
/// nothing in it is read as query syntax; each short one is an escaped `LIKE` pattern.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Query {
    /// What a row must match, and its title too to come first: every long word.
    pub(crate) words: String,
    /// What a row's text must also hold, as patterns for `LIKE … ESCAPE '\'`.
    pub(crate) short: Vec<String>,
}

/// Text past [`MAX_TEXT`] bytes or [`MAX_WORDS`] words.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TooLong;

/// The query for `text`, or none when no word in it is long enough to look up.
pub(crate) fn parse(text: &str) -> Result<Option<Query>, TooLong> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if text.len() > MAX_TEXT || words.len() > MAX_WORDS {
        return Err(TooLong);
    }
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
    }))
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
        let longest = "a".repeat(MAX_TEXT);
        let words = ["word"; MAX_WORDS];

        parse(&longest).unwrap().unwrap();
        assert_eq!(parse(&format!("{longest}a")), Err(TooLong));
        parse(&words.join(" ")).unwrap().unwrap();
        assert_eq!(parse(&format!("{} more", words.join(" "))), Err(TooLong));
    }
}
