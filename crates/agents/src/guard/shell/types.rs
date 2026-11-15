//! The word and simple-command types the parser builds.

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
    /// When `dynamic`, how much of `text` came before the first expansion.
    pub(crate) known: usize,
}

impl Word {
    // From here on the word holds what only the shell knows.
    pub(crate) fn unknown(&mut self) {
        if !self.dynamic {
            self.known = self.text.len();
        }
        self.dynamic = true;
    }

    /// The word made of `value`, the end of this word's text, as after an option's `=`; the
    /// shell expands a `~` that starts it.
    pub(crate) fn tail(&self, value: &str) -> Self {
        Self {
            text: value.to_owned(),
            tilde: value.starts_with('~'),
            known: self
                .known
                .saturating_sub(self.text.len().saturating_sub(value.len())),
            ..self.clone()
        }
    }
}

/// One command with its words and where its output goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Simple {
    pub(crate) words: Vec<Word>,
    /// Files its output is written to.
    pub(crate) writes: Vec<Word>,
    /// Files its input is read from.
    pub(crate) reads: Vec<Word>,
    /// A `cd` in it may not move the commands after it: it runs in a shell of its own, as in a
    /// pipeline, `( … )`, `$( … )` or with `&`, or only on some paths, beside `||` or in a block.
    pub(crate) apart: bool,
}
