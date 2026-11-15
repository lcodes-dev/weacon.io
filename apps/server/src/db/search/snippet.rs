//! A hit's snippet: [`SHOWN`] characters around its first match, cut in linear time however
//! many matches its text holds.

use weacon_protocol::{Mark, Snippet};

use super::{MATCH_END, MATCH_START};
use crate::search::query::fold;

/// How many characters a snippet shows before its first match, and in all.
const BEFORE: usize = 40;
const SHOWN: usize = 200;

/// The snippet around the first of `words` in `body`, each folded as [`fold`] folds; for a body
/// that holds none of them, as where only the title matched, the snippet of `title_marked`,
/// whose matches the index marked.
pub(super) fn of(body: &str, words: &[String], title_marked: &str) -> Snippet {
    let chars: Vec<char> = body.chars().collect();
    let folded: String = chars.iter().copied().map(fold).collect();
    let first = words
        .iter()
        .filter(|word| !word.is_empty())
        .filter_map(|word| folded.find(word.as_str()))
        .min()
        .and_then(|byte| folded.get(..byte))
        .map(|before| before.chars().count());
    let Some(first) = first else {
        let chars: Vec<char> = title_marked.chars().collect();
        let first = chars.iter().position(|ch| *ch == MATCH_START).unwrap_or(0);
        let (start, end) = window(&chars, first);
        let shown: String = chars.get(start..end).unwrap_or_default().iter().collect();
        return snippet(&cut(shown, start, end < chars.len()));
    };
    let (start, end) = window(&chars, first);
    let folded: Vec<char> = folded.chars().collect();
    let words: Vec<Vec<char>> = words.iter().map(|word| word.chars().collect()).collect();
    let shown = marked(&chars, &folded, &words, (start, end));
    snippet(&cut(shown, start, end < chars.len()))
}

/// `chars` from `start` up to `end`, with each match of `words` in `folded` marked; a match
/// right after another joins it, as the index joins its own.
fn marked(
    chars: &[char],
    folded: &[char],
    words: &[Vec<char>],
    (start, end): (usize, usize),
) -> String {
    let mut marked = String::new();
    let mut index = start;
    while index < end {
        let Some(length) = matching(folded, index, words) else {
            marked.extend(chars.get(index));
            index += 1;
            continue;
        };
        let upto = end.min(index + length);
        if marked.ends_with(MATCH_END) {
            marked.pop();
        } else {
            marked.push(MATCH_START);
        }
        marked.extend(chars.get(index..upto).unwrap_or_default());
        marked.push(MATCH_END);
        index = upto;
    }
    marked
}

/// How many characters of the longest of `words` that `folded` holds at `index`, if any.
fn matching(folded: &[char], index: usize, words: &[Vec<char>]) -> Option<usize> {
    words
        .iter()
        .filter(|word| {
            !word.is_empty() && folded.get(index..index + word.len()) == Some(word.as_slice())
        })
        .map(Vec::len)
        .max()
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

/// `shown` with an ellipsis where it was cut, and a match it ends inside closed.
fn cut(mut shown: String, start: usize, more: bool) -> String {
    let open = shown
        .chars()
        .rev()
        .find(|ch| matches!(*ch, MATCH_START | MATCH_END));
    if open == Some(MATCH_START) {
        shown.push(MATCH_END);
    }
    if start > 0 {
        shown.insert_str(0, "… ");
    }
    if more {
        shown.push_str(" …");
    }
    shown
}

/// A snippet's text and marks, from one with its matches between the marking bytes.
fn snippet(marked: &str) -> Snippet {
    let mut snippet = Snippet::default();
    let mut start = None;
    for ch in marked.chars() {
        match ch {
            MATCH_START => start = Some(utf16_len(&snippet.text)),
            MATCH_END => {
                if let Some(start) = start.take() {
                    let end = utf16_len(&snippet.text);
                    snippet.marks.push(Mark { start, end });
                }
            }
            ch => snippet.text.push(ch),
        }
    }
    snippet
}

fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    // `snippet`'s text with each mark in brackets.
    fn shown(snippet: &Snippet) -> String {
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
    fn a_snippet_marks_its_matches_in_utf16_units() {
        let marked = "😀 \u{1}ab\u{2}é \u{1}c\u{2}\u{2}d";

        assert_eq!(
            snippet(marked),
            Snippet {
                text: "😀 abé cd".to_owned(),
                marks: vec![Mark { start: 3, end: 5 }, Mark { start: 7, end: 8 }],
            }
        );
        assert_eq!(snippet("plain").marks, []);
    }

    #[test]
    fn a_body_is_marked_around_its_first_match_whatever_the_case_and_accents() {
        let body = format!("{}Le Café ouvre. Le CAFÉ ferme.", "mot ".repeat(20));

        let found = of(&body, &words(&["cafe"]), "");
        assert!(found.text.starts_with("… mot mot"), "{found:?}");
        assert!(
            shown(&found).ends_with("Le [Café] ouvre. Le [CAFÉ] ferme."),
            "{found:?}"
        );
        assert_eq!(
            shown(&of("ab abc", &words(&["ab", "abc"]), "")),
            "[ab] [abc]"
        );
    }

    #[test]
    fn a_title_snippet_stands_in_when_the_body_holds_no_word() {
        let found = of("nothing here", &words(&["plan"]), "The \u{1}plan\u{2}");

        assert_eq!(shown(&found), "The [plan]");
        assert_eq!(shown(&of("", &words(&["plan"]), "plain")), "plain");
    }

    #[test]
    fn a_snippet_shows_so_many_characters_and_closes_a_match_it_cuts() {
        let body = "a".repeat(SHOWN * 2);

        let found = of(&body, &words(&["aaa"]), "");
        assert_eq!(found.text, format!("{} …", "a".repeat(SHOWN)));
        assert_eq!(found.marks, [Mark { start: 0, end: 200 }]);
    }

    #[test]
    fn many_matches_in_a_whole_body_are_cut_in_one_pass() {
        let body = "aaa ".repeat(64 * 1024);

        let found = of(&body, &words(&["aaa"]), "");
        assert_eq!(found.marks.len(), SHOWN / 4);
        assert!(found.text.ends_with(" …"));
    }
}
