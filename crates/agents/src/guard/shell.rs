//! Splits a shell command line into the simple commands it would run, without running it.

mod quoting;

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
    /// Files its input is read from.
    pub(crate) reads: Vec<Word>,
    /// Reads its input from a file, a pipe or text in the command line.
    pub(crate) fed: bool,
    /// A `cd` in it may not move the commands after it: it runs in a shell of its own, as in a
    /// pipeline, `( … )`, `$( … )` or with `&`, or only on some paths, beside `||` or in a block.
    pub(crate) apart: bool,
}

/// The simple commands in `line`, those inside substitutions included; `None` when it can't be
/// split for sure, as with an unclosed quote.
pub(crate) fn parse(line: &str) -> Option<Vec<Simple>> {
    parse_nested(line, 0)
}

const OPENS: [&str; 6] = ["if", "while", "until", "for", "select", "case"];
const CLOSES: [&str; 3] = ["fi", "done", "esac"];

// Far deeper than any real command; deeper nesting would exhaust the stack.
const MAX_DEPTH: usize = 32;

fn parse_nested(line: &str, depth: usize) -> Option<Vec<Simple>> {
    let mut parser = Parser {
        chars: line.chars().collect(),
        at: 0,
        found: Vec::new(),
        heredocs: Vec::new(),
        depth,
        unsure: false,
        parens: 0,
        blocks: 0,
    };
    parser.list(None)?;
    (!parser.unsure).then_some(parser.found)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    found: Vec<Simple>,
    // Delimiters of here-documents whose text starts at the next line.
    heredocs: Vec<String>,
    // How many substitutions this text is inside.
    depth: usize,
    // How many `(` are open.
    parens: usize,
    // How many `if`, loops and `case`s are open, whose commands may not run.
    blocks: usize,
    // Met something it can split but not follow, such as a `case` inside a substitution.
    unsure: bool,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn next(&mut self) -> Option<char> {
        let char = self.chars.get(self.at).copied()?;
        self.advance(1);
        Some(char)
    }

    fn advance(&mut self, by: usize) {
        self.at += by;
    }

    // Moves past the characters `keep` accepts, in one search rather than a loop.
    fn skip_while(&mut self, keep: impl Fn(char) -> bool) {
        let rest = self.chars.get(self.at..).unwrap_or_default();
        self.advance(
            rest.iter()
                .position(|char| !keep(*char))
                .unwrap_or(rest.len()),
        );
    }

    // The text up to `end`, moving past it; `None` if `end` never comes.
    fn until(&mut self, end: char) -> Option<String> {
        let rest = self.chars.get(self.at..)?;
        let length = rest.iter().position(|char| *char == end)?;
        let text = rest.get(..length)?.iter().collect();
        self.advance(length + 1);
        Some(text)
    }

    // Commands up to `end`, or the end of the line.
    fn list(&mut self, end: Option<char>) -> Option<()> {
        if self.depth > MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let listed = self.commands(end);
        self.depth -= 1;
        listed
    }

    fn commands(&mut self, end: Option<char>) -> Option<()> {
        let mut simple = Simple::default();
        let mut word: Option<Word> = None;
        loop {
            let Some(char) = self.peek() else {
                self.finish(&mut simple, &mut word);
                return end.is_none().then_some(());
            };
            if Some(char) == end {
                self.advance(1);
                self.finish(&mut simple, &mut word);
                return Some(());
            }
            let before = self.at;
            self.step(char, &mut simple, &mut word)?;
            // Every step reads something; this bounds the loop should one ever not.
            if self.at <= before {
                return None;
            }
        }
    }

    fn step(&mut self, char: char, simple: &mut Simple, word: &mut Option<Word>) -> Option<()> {
        match char {
            ' ' | '\t' => {
                self.advance(1);
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
        self.advance(1);
        if char == '&' && self.peek() == Some('>') {
            self.advance(1);
            return self.redirect_target(simple, word, true);
        }
        let pipe = char == '|' && self.peek() != Some('|');
        let or = char == '|' && !pipe;
        let background = char == '&' && self.peek() != Some('&');
        if matches!(
            (char, self.peek()),
            ('&', Some('&')) | ('|', Some('|' | '&'))
        ) {
            self.advance(1);
        }
        simple.apart |= pipe || background || or;
        self.finish(simple, word);
        match char {
            '(' => self.parens += 1,
            ')' => self.parens = self.parens.saturating_sub(1),
            _ => {}
        }
        simple.fed = pipe;
        simple.apart = pipe || or;
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
        self.advance(1);
        if self.peek() == Some('(') {
            // What `<(…)` prints is read as a file, which may be the program's code.
            simple.fed |= char == '<';
            self.advance(1);
            self.list(Some(')'))?;
            return Some(());
        }
        if char == '>' {
            if matches!(self.peek(), Some('>' | '|')) {
                self.advance(1);
            }
            return self.redirect_target(simple, word, true);
        }
        simple.fed = true;
        if self.peek() == Some('<') {
            self.advance(1);
            if self.peek() == Some('<') {
                self.advance(1);
                return self.redirect_target(simple, word, false);
            }
            if self.peek() == Some('-') {
                self.advance(1);
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
        // `>&2` copies a descriptor and `>&-` closes one; after a plain `>`, `2` is a file.
        let duplicates = self.peek() == Some('&');
        if duplicates {
            self.advance(1);
        }
        let target = self.target()?;
        // A word from an expansion may name a file, so only a written number or `-` is a descriptor.
        let descriptor = duplicates
            && !target.dynamic
            && !target.text.is_empty()
            && target
                .text
                .chars()
                .all(|char| char.is_ascii_digit() || char == '-');
        if writes && !descriptor {
            simple.writes.push(target);
        } else if !writes {
            simple.reads.push(target);
        }
        Some(())
    }

    // The word after a redirection.
    fn target(&mut self) -> Option<Word> {
        self.skip_while(|char| matches!(char, ' ' | '\t'));
        let mut word = Word::default();
        while let Some(char) = self.peek() {
            if matches!(
                char,
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
            ) {
                break;
            }
            let before = self.at;
            self.word_part(&mut word)?;
            if self.at <= before {
                return None;
            }
        }
        (!word.text.is_empty() || word.dynamic).then_some(word)
    }

    fn comment(&mut self) {
        self.skip_while(|char| char != '\n');
    }

    // Here-document text is input, not commands, so it is skipped.
    fn skip_heredocs(&mut self) -> Option<()> {
        for delimiter in std::mem::take(&mut self.heredocs) {
            self.skip_heredoc(&delimiter)?;
        }
        Some(())
    }

    fn skip_heredoc(&mut self, delimiter: &str) -> Option<()> {
        loop {
            let before = self.at;
            let line = self.line()?;
            if line.trim_start_matches('\t') == delimiter {
                return Some(());
            }
            // The shell runs substitutions in the text unless its delimiter was quoted, and joins a
            // line ending in `\` to the next, which may then be the delimiter.
            if line.contains("$(") || line.contains('`') || line.ends_with('\\') {
                return None;
            }
            if self.at <= before {
                return None;
            }
        }
    }

    // The rest of the line, and its end.
    fn line(&mut self) -> Option<String> {
        let start = self.at;
        self.skip_while(|char| char != '\n');
        let line: String = self.chars.get(start..self.at)?.iter().collect();
        // The last line may end the text instead; a delimiter never found leaves nothing.
        if self.next().is_none() && start == self.at {
            return None;
        }
        Some(line)
    }

    fn finish(&mut self, simple: &mut Simple, word: &mut Option<Word>) {
        end_word(simple, word);
        let first = simple.words.first().map(|word| word.text.as_str());
        let (opens, closes) = (
            first.is_some_and(|word| OPENS.contains(&word)),
            first.is_some_and(|word| CLOSES.contains(&word)),
        );
        self.blocks += usize::from(opens);
        simple.apart |= self.parens > 0 || self.depth > 1 || self.blocks > 0;
        if closes {
            self.blocks = self.blocks.saturating_sub(1);
        }
        let done = std::mem::take(simple);
        // A `case` pattern's `)` would close the substitution it's in too early.
        if self.depth > 1 && done.words.first().is_some_and(|word| word.text == "case") {
            self.unsure = true;
        }
        if !done.words.is_empty() || !done.writes.is_empty() || !done.reads.is_empty() {
            self.found.push(done);
        }
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
        ] {
            assert!(parse(line).is_none(), "{line}");
        }
        // A stray `)` ends nothing, so the words before it still count.
        let stray = parse("a )").unwrap();
        assert_eq!(stray.iter().map(texts).collect::<Vec<_>>(), [vec!["a"]]);
        assert!(parse("cat <<EOF\nnever ends").is_none());
    }

    #[test]
    fn operators_comments_and_empty_commands_split_as_a_shell_does() {
        assert_eq!(texts(&parse("echo a#b #c").unwrap()[0]), ["echo", "a#b"]);
        let piped = parse("a |& b").unwrap();
        assert_eq!(programs("a |& b"), ["a", "b"]);
        assert!(piped[1].fed);
        assert!(!parse("a && b").unwrap()[1].fed);
        assert_eq!(parse("a;").unwrap().len(), 1);
        assert!(parse(";;").unwrap().is_empty());
        assert_eq!(texts(&parse("echo \"\">out").unwrap()[0]), ["echo", ""]);
    }

    #[test]
    fn quotes_and_expansions_read_as_a_shell_reads_them() {
        let word = |line: &str| parse(line).unwrap().remove(0).words.remove(1);

        assert_eq!(word("echo \"a\\\"b\\$c\\\\d\\e\"").text, "a\"b$c\\d\\e");
        assert_eq!(word("echo \"a\\\nb\"").text, "ab");
        assert!(word("echo $'a b'").dynamic);
        assert!(word("echo $_a").dynamic);
        assert!(word("echo $?").dynamic);
        let literal = word("echo a$/b");
        assert_eq!((literal.text.as_str(), literal.dynamic), ("a$/b", false));
        let arithmetic = parse("echo $((1 + 2)) x").unwrap();
        assert_eq!(
            arithmetic
                .iter()
                .map(|simple| simple.words.len())
                .collect::<Vec<_>>(),
            [3, 3]
        );
    }

    #[test]
    fn nesting_is_bounded_and_counted_from_where_it_is() {
        let nested = |depth: usize, inner: &str| {
            format!(
                "echo {}{inner}{}",
                "$(echo ".repeat(depth),
                ")".repeat(depth)
            )
        };

        assert!(parse(&nested(MAX_DEPTH, "x")).is_some());
        assert!(parse(&nested(MAX_DEPTH + 1, "x")).is_none());
        assert!(parse(&nested(MAX_DEPTH - 2, "`x`")).is_some());
        assert!(parse(&nested(MAX_DEPTH - 1, "`x`")).is_none());
        assert!(parse(&format!("echo {}", "$(a) ".repeat(MAX_DEPTH * 2))).is_some());
    }

    #[test]
    fn a_case_asks_only_inside_a_substitution_and_a_lone_redirection_counts() {
        let top = parse("case x in a) echo a;; esac").unwrap();
        let alone = parse("> out.txt").unwrap();

        assert!(!top.is_empty());
        assert_eq!(alone.len(), 1);
        assert_eq!(alone.first().unwrap().writes.len(), 1);
        assert!(parse("echo $(case x in a) echo a;; esac)").is_none());
    }
}
