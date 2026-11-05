/// Whether a `cfg` predicate holds only in tests: `test`, an `all(...)` with such a part,
/// or an `any(...)` whose parts all are.
pub(crate) fn implies_test(predicate: &str) -> bool {
    // The open calls, each with its name and whether each part so far holds only in tests.
    let mut calls: Vec<(&str, Vec<bool>)> = vec![("", Vec::new())];
    let (mut word, mut part) = ("", None);
    for token in tokens(predicate) {
        match token {
            Token::Word(name) => (word, part) = (name, Some(name == "test")),
            Token::Equals => part = Some(false),
            Token::Text => {}
            Token::Open => {
                calls.push((word, Vec::new()));
                part = None;
            }
            Token::Comma => finish(&mut calls, part.take()),
            Token::Close => {
                finish(&mut calls, part.take());
                part = calls
                    .pop()
                    .map(|(name, parts)| holds_only_in_tests(name, &parts));
            }
        }
    }
    finish(&mut calls, part);
    calls.first().and_then(|(_, parts)| parts.first()) == Some(&true)
}

fn finish(calls: &mut [(&str, Vec<bool>)], part: Option<bool>) {
    if let Some(value) = part
        && let Some((_, parts)) = calls.last_mut()
    {
        parts.push(value);
    }
}

fn holds_only_in_tests(name: &str, parts: &[bool]) -> bool {
    match name {
        "all" => parts.contains(&true),
        "any" => !parts.is_empty() && !parts.contains(&false),
        _ => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    Text,
    Open,
    Close,
    Comma,
    Equals,
}

fn tokens(predicate: &str) -> Vec<Token<'_>> {
    let word = |it: char| it.is_alphanumeric() || it == '_';
    let mut found = Vec::new();
    let mut chars = predicate.char_indices().peekable();
    while let Some((start, it)) = chars.next() {
        match it {
            '(' => found.push(Token::Open),
            ')' => found.push(Token::Close),
            ',' => found.push(Token::Comma),
            '=' => found.push(Token::Equals),
            '"' => {
                chars.by_ref().find(|(_, it)| *it == '"');
                found.push(Token::Text);
            }
            _ if word(it) => {
                let mut end = start + it.len_utf8();
                while let Some((at, it)) = chars.next_if(|(_, it)| word(*it)) {
                    end = at + it.len_utf8();
                }
                found.extend(predicate.get(start..end).map(Token::Word));
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_only_in_tests() {
        for predicate in [
            "test",
            "all(unix, test)",
            "all(unix, all(windows, test))",
            "all(feature = \"x(\", test)",
            "all(not(unix), test)",
            "any(all(test, unix), all(test, windows))",
            "any(test)",
        ] {
            assert!(implies_test(predicate), "{predicate}");
        }
    }

    #[test]
    fn holds_outside_tests_too() {
        for predicate in [
            "",
            "unix",
            "tests",
            "my_test",
            "any(test, unix)",
            "any()",
            "all()",
            "not(test)",
            "not(all(test))",
            "all(not(test))",
            "feature = \"test\"",
            "test = \"integration\"",
            "all(unix, test = \"x\")",
            "any(all(test, unix), windows)",
        ] {
            assert!(!implies_test(predicate), "{predicate}");
        }
    }

    #[test]
    fn splits_into_tokens() {
        assert_eq!(
            tokens("all(a_1, k = \"v, (\")é"),
            [
                Token::Word("all"),
                Token::Open,
                Token::Word("a_1"),
                Token::Comma,
                Token::Word("k"),
                Token::Equals,
                Token::Text,
                Token::Close,
                Token::Word("é"),
            ]
        );
    }

    #[test]
    fn a_deeply_nested_predicate_does_not_overflow_the_stack() {
        let depth = 100_000;
        let predicate = format!("{}test{}", "all(".repeat(depth), ")".repeat(depth));
        assert!(implies_test(&predicate));
        assert!(!implies_test(&predicate.replace("test", "unix")));
    }

    #[test]
    fn unbalanced_parentheses_do_not_hold() {
        assert!(!implies_test("all(test"));
        assert!(!implies_test("test))"));
    }
}
