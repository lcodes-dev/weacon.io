//! The parts of a word: quotes, escapes, expansions and substitutions.

use super::{Parser, Word, parse_nested};

impl Parser {
    pub(super) fn word_part(&mut self, word: &mut Word) -> Option<()> {
        let char = self.next()?;
        match char {
            '\'' => word.text.push_str(&self.until('\'')?),
            '"' => return self.double_quoted(word),
            '\\' => {
                let escaped = self.next()?;
                if escaped != '\n' {
                    word.text.push(escaped);
                }
            }
            '$' => self.expansion(word)?,
            '`' => self.backquoted(word)?,
            '~' if word.text.is_empty() && !word.dynamic => {
                word.tilde = true;
                word.text.push(char);
            }
            '*' | '?' | '[' => {
                word.glob = true;
                word.text.push(char);
            }
            '{' => {
                // With a `,` or `..` inside, the shell expands it to several words.
                let rest = self.chars.get(self.at..).unwrap_or_default();
                let inside: String = rest
                    .iter()
                    .take_while(|next| !matches!(next, '}' | ' ' | '\t' | '\n' | ';' | '|' | '&'))
                    .collect();
                if inside.contains(',') || inside.contains("..") {
                    word.unknown();
                }
                word.text.push(char);
            }
            _ => word.text.push(char),
        }
        Some(())
    }

    fn double_quoted(&mut self, word: &mut Word) -> Option<()> {
        loop {
            let before = self.at;
            match self.next()? {
                '"' => return Some(()),
                '\\' => self.escaped_in_quotes(word)?,
                // Inside double quotes, `$'` and `$"` are plain text.
                '$' if matches!(self.peek(), Some('\'' | '"')) => word.text.push('$'),
                '$' => self.expansion(word)?,
                '`' => self.backquoted(word)?,
                char => word.text.push(char),
            }
            if self.at <= before {
                return None;
            }
        }
    }

    // Inside double quotes a backslash only escapes what the shell would read otherwise.
    fn escaped_in_quotes(&mut self, word: &mut Word) -> Option<()> {
        let escaped = self.next()?;
        if !matches!(escaped, '$' | '`' | '"' | '\\' | '\n') {
            word.text.push('\\');
        }
        if escaped != '\n' {
            word.text.push(escaped);
        }
        Some(())
    }

    // After a `$`: a substitution's commands run, and any expansion makes the word unknown.
    fn expansion(&mut self, word: &mut Word) -> Option<()> {
        match self.peek() {
            // Arithmetic evaluates what variables hold, which can run commands.
            Some('(') if self.peek_at(1) == Some('(') => return None,
            Some('[') => return None,
            Some('(') => {
                self.advance(1);
                self.list(Some(')'))?;
            }
            Some('{') => {
                self.advance(1);
                let name = self.until('}')?;
                // A default value can hold a substitution or quotes, which this doesn't follow.
                if name.contains(['$', '`', '\'', '"', '\\', '{']) {
                    return None;
                }
                home_or_unknown(word, &name);
                return Some(());
            }
            Some(char) if char.is_ascii_alphanumeric() || char == '_' => {
                let start = self.at;
                self.skip_while(|char| char.is_ascii_alphanumeric() || char == '_');
                let name: String = self.chars.get(start..self.at)?.iter().collect();
                home_or_unknown(word, &name);
                return Some(());
            }
            Some(char) if "@*#?$!-".contains(char) => self.advance(1),
            Some('\'') => {
                self.advance(1);
                self.until('\'')?;
            }
            // `$"…"` is text in double quotes.
            Some('"') => {
                self.advance(1);
                return self.double_quoted(word);
            }
            _ => {
                word.text.push('$');
                return Some(());
            }
        }
        word.unknown();
        Some(())
    }

    // The command up to the closing `` ` ``, as the shell reads it: without the `\\` before a
    // `\\`, `` ` `` or `$`.
    fn backquoted_text(&mut self) -> Option<String> {
        let mut text = String::new();
        loop {
            let char = self.next()?;
            if char == '`' {
                return Some(text);
            }
            let escaped = self
                .peek()
                .filter(|next| char == '\\' && matches!(next, '\\' | '`' | '$'));
            if let Some(escaped) = escaped {
                self.advance(1);
                text.push(escaped);
            } else {
                text.push(char);
            }
        }
    }

    fn backquoted(&mut self, word: &mut Word) -> Option<()> {
        let inner = self.backquoted_text()?;
        self.found.extend(parse_nested(&inner, self.depth + 1)?);
        word.unknown();
        Some(())
    }
}

// `$HOME` at the start of a word, or of an option's value after `=` or `@`, is the home
// folder, as `~` is; any other variable is unknown.
fn home_or_unknown(word: &mut Word, name: &str) {
    let starts = word.text.is_empty() || word.text == "@" || word.text.ends_with('=');
    if name == "HOME" && starts && !word.dynamic {
        word.tilde |= word.text.is_empty();
        word.text.push('~');
    } else {
        word.unknown();
    }
}
