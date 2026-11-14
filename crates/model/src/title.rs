const TITLE_MAX: usize = 200;

/// One line of 1 to 200 characters, with no space at either end and nothing that overrides
/// or isolates the direction of the text. Direction marks, which right-to-left text needs,
/// are allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title(String);

impl Title {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Title {
    type Error = String;

    fn try_from(title: String) -> Result<Self, Self::Error> {
        let fits = (1..=TITLE_MAX).contains(&title.chars().count())
            && !title.contains(|char: char| char.is_control() || breaks_or_turns(char));
        if fits && title.trim() == title {
            Ok(Self(title))
        } else {
            Err(title)
        }
    }
}

/// Whether `char` is a line or paragraph separator, or overrides or isolates text direction.
const fn breaks_or_turns(char: char) -> bool {
    matches!(char, '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> Title {
        Title::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn titles_are_one_trimmed_line_of_limited_length() {
        let longest = "é".repeat(TITLE_MAX);
        for good in ["a", "Write the docs", longest.as_str()] {
            assert_eq!(title(good).as_str(), good);
        }
        let too_long = "a".repeat(TITLE_MAX + 1);
        let turned = [
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{202A}b",
            "a\u{202E}b",
            "a\u{2066}b",
            "a\u{2069}b",
        ];
        let marks = ["a\u{61C}b", "a\u{200E}b", "a\u{200F}b"];
        for good in ["a\u{2027}b", "a\u{202F}b", "a\u{2065}b", "a\u{206A}b"]
            .into_iter()
            .chain(marks)
        {
            assert_eq!(title(good).as_str(), good);
        }
        for bad in ["", " ", " a", "a ", "a\nb", "a\tb", too_long.as_str()]
            .into_iter()
            .chain(turned)
        {
            assert_eq!(Title::try_from(bad.to_owned()), Err(bad.to_owned()));
        }
    }
}
