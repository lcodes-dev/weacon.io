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
            '$' => self.expansion(word, false)?,
            '`' => self.backquoted(word, false)?,
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
                    word.splits = true;
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
                '$' => self.expansion(word, true)?,
                '`' => self.backquoted(word, true)?,
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

    // After a `$`: a substitution's commands run, and any expansion makes the word unknown. Unless
    // `quoted`, an expansion also splits the word, so its value can become several words.
    fn expansion(&mut self, word: &mut Word, quoted: bool) -> Option<()> {
        match self.peek() {
            // Arithmetic evaluates what variables hold, which can run commands.
            Some('(') if self.peek_at(1) == Some('(') => return None,
            Some('[') => return None,
            Some('(') => {
                self.advance(1);
                self.list(Some(')'))?;
                word.unknown();
                word.splits |= !quoted;
            }
            Some('{') => {
                self.advance(1);
                let name = self.until('}')?;
                // A default value can hold a substitution or quotes, which this doesn't follow.
                // A subscript is arithmetic, which can run what a variable holds. `${!X}` and
                // `${X:offset}` evaluate a value; `${X@P}` and zsh's `${(e)X}` expand it.
                if name.contains(['$', '`', '\'', '"', '\\', '{', '[']) || evaluates(&name) {
                    return None;
                }
                home_or_unknown(word, &name, quoted);
            }
            Some(char) if char.is_ascii_alphanumeric() || char == '_' => {
                let start = self.at;
                self.skip_while(|char| char.is_ascii_alphanumeric() || char == '_');
                let name: String = self.chars.get(start..self.at)?.iter().collect();
                home_or_unknown(word, &name, quoted);
            }
            // `"$@"` is the one quoted expansion that still splits: it becomes one word per
            // positional parameter, where `"$*"` and the rest are a single word.
            Some(char) if "@*#?$!-".contains(char) => {
                self.advance(1);
                word.unknown();
                word.splits |= !quoted || char == '@';
            }
            // `$'…'` is ANSI-C quoting, where a backslash escapes the next character, `\'` and `\\`
            // included, so the string ends only at an unescaped `'`. It is one literal word.
            Some('\'') => {
                self.advance(1);
                self.skip_ansi_c()?;
                word.unknown();
            }
            // `$"…"` is text in double quotes.
            Some('"') => {
                self.advance(1);
                return self.double_quoted(word);
            }
            _ => word.text.push('$'),
        }
        Some(())
    }

    // Advances past an ANSI-C `$'…'` body to its closing `'`; a backslash escapes the next
    // character, so `\'` doesn't end it.
    fn skip_ansi_c(&mut self) -> Option<()> {
        loop {
            match self.peek() {
                None => return None,
                Some('\'') => {
                    self.advance(1);
                    return Some(());
                }
                Some('\\') => self.advance(2),
                Some(_) => self.advance(1),
            }
        }
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

    fn backquoted(&mut self, word: &mut Word, quoted: bool) -> Option<()> {
        let inner = self.backquoted_text()?;
        self.found.extend(parse_nested(&inner, self.depth + 1)?);
        word.unknown();
        word.splits |= !quoted;
        Some(())
    }
}

// Whether a `${…}` body has the shell evaluate or expand what a variable holds, rather than just
// read it: an indirect `!`, a zsh flag `(`, an `@` operator, or a `:` offset (arithmetic).
fn evaluates(body: &str) -> bool {
    let operator = body
        .trim_start_matches('#')
        .trim_start_matches(|char: char| char.is_ascii_alphanumeric() || char == '_');
    body.starts_with(['!', '('])
        || operator.starts_with('@')
        || operator
            .strip_prefix(':')
            .is_some_and(|rest| !rest.starts_with(['-', '=', '?', '+']))
}

// `$HOME` at the start of a word, or of an option's value after `=` or `@`, is the home
// folder, as `~` is; any other variable is unknown.
fn home_or_unknown(word: &mut Word, name: &str, quoted: bool) {
    let starts = word.text.is_empty() || word.text == "@" || word.text.ends_with('=');
    if name == "HOME" && starts && !word.dynamic {
        word.tilde |= word.text.is_empty();
        word.text.push('~');
    } else {
        word.unknown();
        word.splits |= !quoted;
    }
}
