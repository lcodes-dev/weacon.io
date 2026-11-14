//! What each simple command does that a person must be asked about: its redirections, its
//! variables and the files it reads, then the table of what is known safe.

use std::path::{Path, PathBuf};

use super::shell::{Simple, Word};
use super::words::{
    Command, begins, command, is_assignment, positional, program_name, short_flags,
};
use super::{Context, Reason, check_word, git, path_of, reading, safe};

// Output that goes to no file.
const DEVICES: [&str; 5] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty", "nul"];
// Of the variables that point git at another repository or have it run a command, these do
// neither.
const GIT_HARMLESS: [&str; 2] = ["GIT_PAGER", "GIT_TERMINAL_PROMPT"];
// Variables that choose which programs run or how the shell splits words.
// Variables that choose which programs run or how the shell splits words; an editor or pager
// is a program the command then runs.
const SHELL_SETTINGS: [&str; 8] = [
    "PATH",
    "BASH_ENV",
    "ENV",
    "IFS",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "SUDO_EDITOR",
];
// Programs that print their words and read no file they name, so an expansion there is only text.
const TEXT_ONLY: [&str; 3] = ["echo", "printf", "git"];
// Programs that read through every folder they're given.
const DEEP: [&str; 11] = [
    "tar", "zip", "7z", "rsync", "rg", "ag", "ack", "scp", "rclone", "restic", "borg",
];

/// What `simple`, run in `cwd`, needs a person for; a `cd` moves `cwd` for what follows.
pub(super) fn judge(simple: &Simple, context: &Context, cwd: &mut PathBuf) -> Option<Reason> {
    let redirected = simple
        .writes
        .iter()
        .filter(|target| !DEVICES.contains(&target.text.to_ascii_lowercase().as_str()))
        .find_map(|target| check_word(target, context, cwd, Reason::WriteOutside))
        .or_else(|| {
            simple
                .reads
                .iter()
                .find_map(|source| read(source, context, cwd, false, true))
        })
        .or_else(|| assignments(simple, context, cwd));
    if let Some(reason) = redirected {
        return Some(reason);
    }
    let words = match command(&simple.words) {
        Command::Empty => return None,
        Command::Unsure => return Some(Reason::Unsure),
        Command::Runs(words) => words,
    };
    let program = words.first()?;
    if program.dynamic {
        return Some(Reason::Unsure);
    }
    let name = program_name(&program.text);
    let args = words.get(1..).unwrap_or_default();
    if context
        .additions
        .iter()
        .any(|pattern| begins(&name, args, pattern))
    {
        return Some(Reason::Addition);
    }
    let deep = goes_deep(&name, args);
    let strict = !TEXT_ONLY.contains(&name.as_str());
    let reads = (!matches!(name.as_str(), "cd" | "pushd"))
        .then(|| {
            words
                .iter()
                .find_map(|word| read(word, context, cwd, deep, strict))
        })
        .flatten();
    let call = Call {
        name: &name,
        args,
        simple,
        context,
    };
    call.judge(cwd).or(reads)
}

// Whether the program reads through the folders it's given, so naming one above a credential
// folder reaches it.
fn goes_deep(name: &str, args: &[Word]) -> bool {
    // `find` only lists what it finds; running commands on it asks anyway.
    name != "find"
        && (DEEP.contains(&name)
            || args.iter().any(|arg| {
                // `--recursive` and `--dereference-recursive`, shortened as the tools allow.
                (arg.text.starts_with("--") && arg.text.contains("rec"))
                    || short_flags(&arg.text).is_some_and(|flags| flags.contains(['r', 'R']))
                    || (name == "cp"
                        && short_flags(&arg.text).is_some_and(|flags| flags.contains('a')))
            }))
}

// A variable set to a credential's path is read through later; `GIT_` ones redirect git.
fn assignments(simple: &Simple, context: &Context, cwd: &Path) -> Option<Reason> {
    simple
        .words
        .iter()
        .filter(|word| is_assignment(word))
        .find_map(|word| {
            let (name, _) = word.text.split_once('=')?;
            let redirects_git = name.starts_with("GIT_") && !GIT_HARMLESS.contains(&name);
            if redirects_git || SHELL_SETTINGS.contains(&name) {
                return Some(Reason::Unsure);
            }
            // A value made wholly by the shell is checked where it is used.
            let value = word.tail(word.text.get(name.len() + 1..)?);
            if value.dynamic && value.known == 0 {
                return None;
            }
            read_path(&value, context, cwd, false, false)
        })
}

// A word may name a file itself, or as an option's value, as in `--file=…` or `-d @…`.
// `strict` when the program may read a file it's given, so a word only the shell can spell asks.
fn read(word: &Word, context: &Context, cwd: &Path, deep: bool, strict: bool) -> Option<Reason> {
    let value = word.text.split_once('=').map(|(_, value)| value);
    let value = value
        .or_else(|| word.text.strip_prefix('@'))
        .filter(|value| !value.is_empty());
    let inner = value.map(|value| word.tail(value));
    inner
        .and_then(|inner| read_path(&inner, context, cwd, deep, strict))
        .or_else(|| read_path(word, context, cwd, deep, strict))
}

// A wildcard can match a credential folder below where it starts, and so can an expansion
// below the folder written before it.
fn read_path(
    word: &Word,
    context: &Context,
    cwd: &Path,
    deep: bool,
    strict: bool,
) -> Option<Reason> {
    if let Some(path) = path_of(word, context, cwd) {
        return reading(&path, context, deep || word.glob);
    }
    if !word.dynamic {
        return Some(Reason::Unsure);
    }
    // Only the text before the first expansion is known; one that starts a path may be anywhere.
    let before = word.text.get(..word.known)?;
    let Some((folder, _)) = before.rsplit_once('/') else {
        let anywhere = word.text.contains('/') || (strict && before.is_empty());
        return anywhere.then_some(Reason::Unsure);
    };
    let known = Word {
        text: if folder.is_empty() {
            "/".to_owned()
        } else {
            folder.to_owned()
        },
        dynamic: false,
        glob: false,
        ..word.clone()
    };
    reading(&path_of(&known, context, cwd)?, context, true)
}

struct Call<'a> {
    name: &'a str,
    args: &'a [Word],
    simple: &'a Simple,
    context: &'a Context,
}

impl Call<'_> {
    fn judge(&self, cwd: &mut PathBuf) -> Option<Reason> {
        match self.name {
            "cd" | "pushd" if self.simple.apart => Some(Reason::Unsure),
            "cd" | "pushd" => self.change_folder(cwd),
            "git" => git::judge(self.args, self.context, cwd)
                .or_else(|| safe::judge(self.name, self.args, self.context, cwd)),
            "cp" | "mv" | "ln" => self
                .landed(cwd)
                .or_else(|| safe::judge(self.name, self.args, self.context, cwd)),
            _ => safe::judge(self.name, self.args, self.context, cwd),
        }
    }

    // Into an existing folder, each source lands at its own name there, which may be a symlink.
    fn landed(&self, cwd: &Path) -> Option<Reason> {
        let named = positional(self.args);
        let (folder, sources) = named.split_last()?;
        self.landing(folder, sources, cwd)
            .iter()
            .find_map(|target| check_word(target, self.context, cwd, Reason::WriteOutside))
    }

    // `cd -` goes back to a folder only the shell knows, and with `CDPATH` a plain name may be
    // found elsewhere.
    fn change_folder(&self, cwd: &mut PathBuf) -> Option<Reason> {
        let target = positional(self.args);
        let searched = |word: &&&Word| {
            self.context.cdpath && !word.tilde && !word.text.starts_with(['.', '/'])
        };
        if self.args.iter().any(|arg| arg.text == "-")
            || target.first().is_some_and(|word| searched(&word))
        {
            return Some(Reason::Unsure);
        }
        let folder = match target.first() {
            Some(word) => path_of(word, self.context, cwd),
            None => self.context.home.clone(),
        };
        let Some(folder) = folder.filter(|folder| folder.is_dir()) else {
            return Some(Reason::Unsure);
        };
        *cwd = folder;
        None
    }

    // Into an existing folder, each source lands at its own name there, which may be a symlink.
    fn landing(&self, folder: &Word, sources: &[&Word], cwd: &Path) -> Vec<Word> {
        let is_folder = path_of(folder, self.context, cwd).is_some_and(|path| path.is_dir());
        if !is_folder {
            return Vec::new();
        }
        sources
            .iter()
            .map(|source| {
                let name = source
                    .text
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default();
                Word {
                    text: format!("{}/{name}", folder.text.trim_end_matches('/')),
                    dynamic: folder.dynamic || source.dynamic,
                    glob: folder.glob || source.glob,
                    ..folder.clone()
                }
            })
            .collect()
    }
}
