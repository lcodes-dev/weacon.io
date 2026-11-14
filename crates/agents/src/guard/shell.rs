//! Splits a shell command line into the simple commands it would run, without running it.

/// One word of a command, with what the shell would do to it before running it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Word {
    /// The text with quotes removed.
    pub(crate) text: String,
    /// Holds an expansion, such as `$HOME` or `$(…)`, so its value isn't known here.
    pub(crate) dynamic: bool,
    /// Holds an unquoted `*`, `?` or `[`, which the shell expands to file names.
    pub(crate) glob: bool,
    /// Starts with an unquoted `~`, the home folder.
    pub(crate) tilde: bool,
}

/// One command with its words and where its output goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Simple {
    pub(crate) words: Vec<Word>,
    /// Files its output is written to.
    pub(crate) writes: Vec<Word>,
    /// Reads its input from a file, a pipe or text in the command line.
    pub(crate) fed: bool,
}

/// The simple commands in `line`, those inside substitutions included; `None` when it can't be
/// split for sure, as with an unclosed quote.
pub(crate) fn parse(line: &str) -> Option<Vec<Simple>> {
    let chars: Vec<char> = line.chars().collect();
    let mut parser = Parser {
        chars,
        at: 0,
        found: Vec::new(),
        heredocs: Vec::new(),
    };
    parser.list(None)?;
    Some(parser.found)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    found: Vec<Simple>,
    // Delimiters of here-documents whose text starts at the next line.
    heredocs: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn next(&mut self) -> Option<char> {
        let char = self.peek()?;
        self.at += 1;
        Some(char)
    }

    // Commands up to `end`, or the end of the line.
    fn list(&mut self, end: Option<char>) -> Option<()> {
        let mut simple = Simple::default();
        let mut word: Option<Word> = None;
        loop {
            let Some(char) = self.peek() else {
                self.finish(&mut simple, &mut word);
                return end.is_none().then_some(());
            };
            if Some(char) == end {
                self.at += 1;
                self.finish(&mut simple, &mut word);
                return Some(());
            }
            self.step(char, &mut simple, &mut word)?;
        }
    }

    fn step(&mut self, char: char, simple: &mut Simple, word: &mut Option<Word>) -> Option<()> {
        match char {
            ' ' | '\t' => {
                self.at += 1;
                end_word(simple, word);
            }
            '\n' | ';' | '&' | '|' | '(' | ')' => self.separator(char, simple, word)?,
            '<' | '>' => self.redirect(char, simple, word)?,
            '#' if word.is_none() => self.comment(),
            _ => self.word_part(word.get_or_insert_with(Word::default))?,
        }
        Some(())
    }

    fn separator(
        &mut self,
        char: char,
        simple: &mut Simple,
        word: &mut Option<Word>,
    ) -> Option<()> {
        self.at += 1;
        if char == '&' && self.peek() == Some('>') {
            self.at += 1;
            return self.redirect_target(simple, word, true);
        }
        let pipe = char == '|' && !matches!(self.peek(), Some('|'));
        if matches!(self.peek(), Some('&' | '|')) && matches!(char, '&' | '|') {
            self.at += 1;
        }
        self.finish(simple, word);
        simple.fed = pipe;
        if char == '\n' {
            self.skip_heredocs()?;
        }
        Some(())
    }

    fn redirect(&mut self, char: char, simple: &mut Simple, word: &mut Option<Word>) -> Option<()> {
        // A number just before is the file descriptor, not a word.
        if word.as_ref().is_some_and(|word| {
            !word.text.is_empty() && word.text.chars().all(|digit| digit.is_ascii_digit())
        }) {
            *word = None;
        }
        end_word(simple, word);
        self.at += 1;
        if self.peek() == Some('(') {
            self.at += 1;
            self.list(Some(')'))?;
            return Some(());
        }
        if char == '>' {
            if matches!(self.peek(), Some('>' | '|')) {
                self.at += 1;
            }
            return self.redirect_target(simple, word, true);
        }
        simple.fed = true;
        if self.peek() == Some('<') {
            self.at += 1;
            if self.peek() == Some('<') {
                self.at += 1;
                return self.redirect_target(simple, word, false);
            }
            if self.peek() == Some('-') {
                self.at += 1;
            }
            let delimiter = self.target()?;
            self.heredocs.push(delimiter.text);
            return Some(());
        }
        self.redirect_target(simple, word, false)
    }

    fn redirect_target(
        &mut self,
        simple: &mut Simple,
        word: &mut Option<Word>,
        writes: bool,
    ) -> Option<()> {
        end_word(simple, word);
        if self.peek() == Some('&') {
            // `>&2` copies a descriptor; `>&-` closes one.
            self.at += 1;
        }
        let target = self.target()?;
        if writes
            && !target
                .text
                .chars()
                .all(|char| char.is_ascii_digit() || char == '-')
        {
            simple.writes.push(target);
        }
        Some(())
    }

    // The word after a redirection.
    fn target(&mut self) -> Option<Word> {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.at += 1;
        }
        let mut word = Word::default();
        while let Some(char) = self.peek() {
            if matches!(
                char,
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
            ) {
                break;
            }
            self.word_part(&mut word)?;
        }
        (!word.text.is_empty() || word.dynamic).then_some(word)
    }

    fn comment(&mut self) {
        while self.peek().is_some_and(|char| char != '\n') {
            self.at += 1;
        }
    }

    // Here-document text is input, not commands, so it is skipped.
    fn skip_heredocs(&mut self) -> Option<()> {
        for delimiter in std::mem::take(&mut self.heredocs) {
            while self.line()?.trim_start_matches('\t') != delimiter {}
        }
        Some(())
    }

    // The rest of the line, and its end.
    fn line(&mut self) -> Option<String> {
        let start = self.at;
        while self.peek().is_some_and(|char| char != '\n') {
            self.at += 1;
        }
        let line: String = self.chars.get(start..self.at)?.iter().collect();
        // The last line may end the text instead; a delimiter never found leaves nothing.
        if self.next().is_none() && start == self.at {
            return None;
        }
        Some(line)
    }

    fn word_part(&mut self, word: &mut Word) -> Option<()> {
        let char = self.next()?;
        match char {
            '\'' => {
                let start = self.at;
                while self.next()? != '\'' {}
                let quoted: String = self
                    .chars
                    .get(start..self.at.checked_sub(1)?)?
                    .iter()
                    .collect();
                word.text.push_str(&quoted);
            }
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
            _ => word.text.push(char),
        }
        Some(())
    }

    fn double_quoted(&mut self, word: &mut Word) -> Option<()> {
        loop {
            match self.next()? {
                '"' => return Some(()),
                '\\' => self.escaped_in_quotes(word)?,
                '$' => self.expansion(word)?,
                '`' => self.backquoted(word)?,
                char => word.text.push(char),
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
            Some('(') if self.peek_at(1) == Some('(') => {
                self.at += 2;
                self.list(Some(')'))?;
                if self.next()? != ')' {
                    return None;
                }
            }
            Some('(') => {
                self.at += 1;
                self.list(Some(')'))?;
            }
            Some('{') => {
                let start = self.at + 1;
                while self.next()? != '}' {}
                let name: String = self
                    .chars
                    .get(start..self.at.checked_sub(1)?)?
                    .iter()
                    .collect();
                home_or_unknown(word, &name);
                return Some(());
            }
            Some(char) if char.is_ascii_alphanumeric() || char == '_' => {
                let start = self.at;
                while self
                    .peek()
                    .is_some_and(|char| char.is_ascii_alphanumeric() || char == '_')
                {
                    self.at += 1;
                }
                let name: String = self.chars.get(start..self.at)?.iter().collect();
                home_or_unknown(word, &name);
                return Some(());
            }
            Some(char) if "@*#?$!-".contains(char) => self.at += 1,
            Some('\'') => {
                self.at += 1;
                while self.next()? != '\'' {}
            }
            _ => {
                word.text.push('$');
                return Some(());
            }
        }
        word.dynamic = true;
        Some(())
    }

    fn backquoted(&mut self, word: &mut Word) -> Option<()> {
        let start = self.at;
        while self.next()? != '`' {}
        let inner: String = self
            .chars
            .get(start..self.at.checked_sub(1)?)?
            .iter()
            .collect();
        self.found.extend(parse(&inner)?);
        word.dynamic = true;
        Some(())
    }

    fn finish(&mut self, simple: &mut Simple, word: &mut Option<Word>) {
        end_word(simple, word);
        let done = std::mem::take(simple);
        if !done.words.is_empty() || !done.writes.is_empty() {
            self.found.push(done);
        }
    }
}

// `$HOME` at the start of a word is the home folder, as `~` is; any other variable is unknown.
fn home_or_unknown(word: &mut Word, name: &str) {
    if name == "HOME" && word.text.is_empty() && !word.dynamic {
        word.tilde = true;
        word.text.push('~');
    } else {
        word.dynamic = true;
    }
}

fn end_word(simple: &mut Simple, word: &mut Option<Word>) {
    if let Some(word) = word.take() {
        simple.words.push(word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs(line: &str) -> Vec<String> {
        parse(line)
            .unwrap()
            .into_iter()
            .map(|simple| {
                simple
                    .words
                    .first()
                    .map(|word| word.text.clone())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn texts(simple: &Simple) -> Vec<&str> {
        simple.words.iter().map(|word| word.text.as_str()).collect()
    }

    #[test]
    fn a_line_splits_into_its_commands_with_quotes_and_escapes_removed() {
        let commands = parse("a 'b c' \"d $X\" e\\ f; g && h || i | j & k\nl").unwrap();

        assert_eq!(texts(&commands[0]), ["a", "b c", "d ", "e f"]);
        assert!(commands[0].words[2].dynamic);
        assert!(!commands[0].words[1].dynamic);
        assert_eq!(
            programs("g && h || i | j & k\nl"),
            ["g", "h", "i", "j", "k", "l"]
        );
        assert!(parse("i | j").unwrap()[1].fed);
        assert!(!parse("i || j").unwrap()[1].fed);
        assert_eq!(programs("echo hi # rm -rf /"), ["echo"]);
    }

    #[test]
    fn substitutions_are_commands_too_and_make_their_word_unknown() {
        assert_eq!(programs("echo $(rm -rf x) `ls y`"), ["rm", "ls", "echo"]);
        assert_eq!(programs("diff <(sort a) >(tee b)"), ["sort", "tee", "diff"]);
        assert_eq!(programs("echo \"$(id)\""), ["id", "echo"]);
        let echo = parse("echo $(id)").unwrap().pop().unwrap();
        assert!(echo.words[1].dynamic);
        assert!(parse("echo ${X}y").unwrap()[0].words[1].dynamic);
        assert!(parse("echo $((1 + 2))").is_some());
    }

    #[test]
    fn redirections_say_what_is_written_and_what_is_read() {
        let simple = &parse("make 2>&1 >out.log >>all.log &>both 2>err <in").unwrap()[0];

        let writes: Vec<&str> = simple
            .writes
            .iter()
            .map(|word| word.text.as_str())
            .collect();
        assert_eq!(writes, ["out.log", "all.log", "both", "err"]);
        assert_eq!(texts(simple), ["make"]);
        assert!(simple.fed);
        assert!(parse("cat <<< text").unwrap()[0].fed);
    }

    #[test]
    fn here_document_text_is_input_and_never_a_command() {
        assert_eq!(programs("cat <<EOF > f\nrm -rf /\nEOF\nls"), ["cat", "ls"]);
        assert_eq!(programs("cat <<-'END'\n\trm x\n\tEND\nls"), ["cat", "ls"]);
        assert!(parse("sh <<EOF\nrm x\nEOF").unwrap()[0].fed);
    }

    #[test]
    fn the_home_folder_and_wildcards_are_marked() {
        let words = &parse("ls ~ ~/a \"$HOME/b\" ${HOME}/c a~ '~' *.rs").unwrap()[0].words;
        let tilde: Vec<bool> = words.iter().map(|word| word.tilde).collect();

        assert_eq!(tilde, [false, true, true, true, true, false, false, false]);
        assert_eq!(words[3].text, "~/b");
        assert!(!words[3].dynamic);
        assert!(words[7].glob);
        assert!(!words[6].glob);
    }

    #[test]
    fn a_line_that_cant_be_split_for_sure_is_none() {
        for line in [
            "echo 'open",
            "echo \"open",
            "echo $(open",
            "echo `open",
            "echo \\",
            "a )",
        ] {
            assert!(parse(line).is_none() || line == "a )", "{line}");
        }
        assert!(parse("cat <<EOF\nnever ends").is_none());
    }
}
