//! A hit's snippet: [`SHOWN`] characters around its first match, each match in them marked,
//! found in linear time however many matches its text holds.

use weacon_protocol::{Mark, Snippet};

use crate::search::fold::fold;
use crate::search::query::Marks;

/// How many characters a snippet shows before its first match, and in all.
const BEFORE: usize = 40;
const SHOWN: usize = 200;

/// The snippet around the first match in `body`, or in `title` when the body holds none, as
/// where only the title matched; a title without one shows as it is.
pub(super) fn of(body: &str, title: &str, marks: &Marks) -> Snippet {
    around(body, marks)
        .or_else(|| around(title, marks))
        .unwrap_or_else(|| shown(&Text::new(title), (0, SHOWN), &[]))
}

/// `text` as each kind of word compares it, one character for one.
struct Text {
    chars: Vec<char>,
    folded: Vec<char>,
    ascii: Vec<char>,
}

impl Text {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        Self {
            folded: chars.iter().copied().map(fold).collect(),
            ascii: chars.iter().map(char::to_ascii_lowercase).collect(),
            chars,
        }
    }

    /// The longest word of `marks` at `index`, as long as it is.
    fn matching(&self, index: usize, marks: &Marks) -> Option<usize> {
        let at = |text: &[char], word: &String| {
            let word: Vec<char> = word.chars().collect();
            let found = !word.is_empty() && text.get(index..index + word.len()) == Some(&word[..]);
            found.then_some(word.len())
        };
        let long = marks.long.iter().filter_map(|word| at(&self.folded, word));
        let short = marks.short.iter().filter_map(|word| at(&self.ascii, word));
        long.chain(short).max()
    }
}

/// The snippet of `text` around its first match, if it has one.
fn around(text: &str, marks: &Marks) -> Option<Snippet> {
    let text = Text::new(text);
    let first = first_match(&text, marks)?;
    let window = window(&text.chars, first);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for index in window.0..window.1 {
        let Some(length) = text.matching(index, marks) else {
            continue;
        };
        let upto = window.1.min(index + length);
        // A match that overlaps or touches the one before joins it.
        match ranges.last_mut() {
            Some(last) if index <= last.1 => last.1 = last.1.max(upto),
            _ => ranges.push((index, upto)),
        }
    }
    Some(shown(&text, window, &ranges))
}

/// Where the first word of `marks` is in `text`, found by one linear search for each word.
fn first_match(text: &Text, marks: &Marks) -> Option<usize> {
    let find = |chars: &[char], word: &String| {
        let haystack: String = chars.iter().collect();
        let byte = (!word.is_empty())
            .then(|| haystack.find(word.as_str()))
            .flatten()?;
        haystack.get(..byte).map(|before| before.chars().count())
    };
    let long = marks
        .long
        .iter()
        .filter_map(|word| find(&text.folded, word));
    let short = marks
        .short
        .iter()
        .filter_map(|word| find(&text.ascii, word));
    long.chain(short).min()
}

/// The characters a snippet shows: from a word at most [`BEFORE`] before `first`, [`SHOWN`] in all.
fn window(chars: &[char], first: usize) -> (usize, usize) {
    let earliest = first.saturating_sub(BEFORE);
    let start = match chars.get(earliest..first) {
        Some(before) if earliest > 0 => before
            .iter()
            .position(|ch| ch.is_whitespace())
            .map_or(first, |space| earliest + space + 1),
        _ => 0,
    };
    (start, chars.len().min(start + SHOWN))
}

/// `text` from `start` up to `end`, with an ellipsis where it was cut, and `ranges` as marks in
/// UTF-16 units.
fn shown(text: &Text, (start, end): (usize, usize), ranges: &[(usize, usize)]) -> Snippet {
    let end = end.min(text.chars.len());
    let lead = if start > 0 { "… " } else { "" };
    let units = |from: usize, upto: usize| -> u32 {
        let chars = text.chars.get(from..upto).unwrap_or_default();
        let count: usize = chars.iter().map(|ch| ch.len_utf16()).sum();
        u32::try_from(count + lead.encode_utf16().count()).unwrap_or(u32::MAX)
    };
    let marks = ranges
        .iter()
        .map(|(from, upto)| Mark {
            start: units(start, *from),
            end: units(start, *upto),
        })
        .collect();
    let body: String = text
        .chars
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .collect();
    let tail = if end < text.chars.len() { " …" } else { "" };
    Snippet {
        text: format!("{lead}{body}{tail}"),
        marks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(long: &[&str], short: &[&str]) -> Marks {
        Marks {
            long: long
                .iter()
                .map(|word| word.chars().map(fold).collect())
                .collect(),
            short: short.iter().map(|word| word.to_ascii_lowercase()).collect(),
        }
    }

    // `snippet`'s text with each mark in brackets.
    fn bracketed(snippet: &Snippet) -> String {
        let units: Vec<u16> = snippet.text.encode_utf16().collect();
        let mut out = Vec::new();
        let mut at = 0;
        for Mark { start, end } in &snippet.marks {
            let (start, end) = (
                usize::try_from(*start).unwrap(),
                usize::try_from(*end).unwrap(),
            );
            out.extend_from_slice(&units[at..start]);
            out.extend("[".encode_utf16());
            out.extend_from_slice(&units[start..end]);
            out.extend("]".encode_utf16());
            at = end;
        }
        out.extend_from_slice(&units[at..]);
        String::from_utf16(&out).unwrap()
    }

    #[test]
    fn a_body_is_marked_around_its_first_match_whatever_its_case_and_accents() {
        let body = format!(
            "{}Le Café ouvre. Le CAFÉ ferme près de la řeka.",
            "mot ".repeat(20)
        );

        let found = of(&body, "Notes", &marks(&["cafe", "reka"], &[]));
        assert!(found.text.starts_with("… mot mot"), "{found:?}");
        assert!(
            bracketed(&found).ends_with("Le [Café] ouvre. Le [CAFÉ] ferme près de la [řeka]."),
            "{found:?}"
        );
        assert_eq!(
            bracketed(&of("😀 abc", "", &marks(&["abc"], &[]))),
            "😀 [abc]"
        );
    }

    #[test]
    fn overlapping_and_touching_matches_are_one_mark() {
        assert_eq!(
            bracketed(&of("banana", "", &marks(&["ana"], &[]))),
            "b[anana]"
        );
        assert_eq!(
            bracketed(&of("ab abc", "", &marks(&["abc"], &["ab"]))),
            "[ab] [abc]"
        );
        assert_eq!(
            bracketed(&of("abcabc", "", &marks(&["abc"], &[]))),
            "[abcabc]"
        );
    }

    #[test]
    fn short_words_match_as_typed_ignoring_only_ascii_case() {
        let short = marks(&["plan"], &["é", "v2"]);

        assert_eq!(bracketed(&of("evidence", "Plan é", &short)), "[Plan] [é]");
        assert_eq!(
            bracketed(&of("", "Release V2", &marks(&["release"], &["v2"]))),
            "[Release] [V2]"
        );
        assert_eq!(bracketed(&of("", "Notes", &short)), "Notes");
    }

    #[test]
    fn a_snippet_shows_so_many_characters_and_cuts_a_long_match() {
        let found = of(&"a".repeat(SHOWN * 2), "", &marks(&["aaa"], &[]));
        assert_eq!(found.text, format!("{} …", "a".repeat(SHOWN)));
        assert_eq!(found.marks, [Mark { start: 0, end: 200 }]);

        let late = format!("{}plan {}", "word ".repeat(20), "x".repeat(SHOWN));
        let title = of("", &late, &marks(&["plan"], &[]));
        assert!(title.text.starts_with("… word word"), "{title:?}");
        assert!(title.text.ends_with(" …"), "{title:?}");
        assert_eq!(bracketed(&title).matches("[plan]").count(), 1);
    }

    #[test]
    fn many_matches_in_a_whole_body_are_found_in_one_pass() {
        let body = "aaa ".repeat(64 * 1024);

        let found = of(&body, "", &marks(&["aaa"], &[]));
        assert_eq!(found.marks.len(), SHOWN / 4);
        assert!(found.text.ends_with(" …"));
    }
}
