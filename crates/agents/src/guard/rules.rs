//! What each simple command does that a person must be asked about.

use std::path::{Path, PathBuf};

use super::shell::{Simple, Word};
use super::words::{
    Command, begins, command, in_place, is_assignment, positional, program_name, short_flags,
    value_of, values_of,
};
use super::{Context, Reason, check_word, code, git, path_of, reading};

const DELETERS: [&str; 7] = [
    "rm",
    "rmdir",
    "unlink",
    "srm",
    "trash",
    "trash-put",
    "rimraf",
];
const COPIERS: [&str; 4] = ["cp", "install", "rsync", "ln"];
// Programs whose option names the file or folder they write.
const OUTPUTS: [(&str, &[&str]); 4] = [
    ("curl", &["-o", "--output", "--output-dir"]),
    (
        "wget",
        &["-O", "--output-document", "-P", "--directory-prefix"],
    ),
    ("tar", &["-C", "--directory"]),
    ("unzip", &["-d"]),
];
// `shred` overwrites what it names before it removes anything.
const WRITERS: [&str; 10] = [
    "tee", "touch", "mkdir", "truncate", "chmod", "chown", "chgrp", "chattr", "setfacl", "shred",
];
// Output that goes to no file.
const DEVICES: [&str; 5] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty", "nul"];
// Of the variables that point git at another repository or have it run a command, these do
// neither.
const GIT_HARMLESS: [&str; 2] = ["GIT_PAGER", "GIT_TERMINAL_PROMPT"];
// Variables that choose which programs run or how the shell splits words.
const SHELL_SETTINGS: [&str; 4] = ["PATH", "BASH_ENV", "ENV", "IFS"];
// Programs that print their words and read no file they name, so an expansion there is only text.
const TEXT_ONLY: [&str; 3] = ["echo", "printf", "git"];
const FIND_RUNS: [&str; 4] = ["-exec", "-execdir", "-ok", "-okdir"];
// Programs that read through every folder they're given.
const DEEP: [&str; 11] = [
    "tar", "zip", "7z", "rsync", "rg", "ag", "ack", "scp", "rclone", "restic", "borg",
];
const FIND_WRITES: [&str; 4] = ["-fprint", "-fprint0", "-fprintf", "-fls"];

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
    let (words, partial) = match command(&simple.words) {
        Command::Empty => return None,
        Command::Unsure => return Some(Reason::Unsure),
        Command::Runs { words, partial } => (words, partial),
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
        partial,
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
                arg.text == "--recursive"
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
    // Run through `xargs`, so some of its words come from its input.
    partial: bool,
}

impl Call<'_> {
    fn judge(&self, cwd: &mut PathBuf) -> Option<Reason> {
        let name = self.name;
        match name {
            "weacon" => Some(Reason::Weacon),
            // In a shell of its own, a `cd` moves nothing after it.
            "cd" | "pushd" if self.simple.apart => Some(Reason::Unsure),
            "cd" | "pushd" => self.change_folder(cwd),
            "popd" => Some(Reason::Unsure),
            "git" if self.partial => Some(Reason::Unsure),
            "git" => git::judge(self.args, self.context, cwd),
            "mv" => {
                let mut targets = positional(self.args);
                let into = value_of(self.args, &["-t", "--target-directory"]);
                targets.extend(into.as_ref());
                self.targets(cwd, Reason::DeleteOutside, &targets)
            }
            "dd" => self.dd(cwd),
            "find" => self.find(cwd),
            _ if DELETERS.contains(&name) => {
                self.targets(cwd, Reason::DeleteOutside, &positional(self.args))
            }
            _ if COPIERS.contains(&name) => self.copy(cwd),
            _ if WRITERS.contains(&name) || (name == "sed" && in_place(self.args)) => {
                self.targets(cwd, Reason::WriteOutside, &positional(self.args))
            }
            _ if let Some((_, options)) = OUTPUTS.iter().find(|(program, _)| *program == name) => {
                let outputs = values_of(self.args, options);
                self.targets(
                    cwd,
                    Reason::WriteOutside,
                    &outputs.iter().collect::<Vec<_>>(),
                )
            }
            _ => code::judge(name, self.args, self.simple),
        }
    }

    fn targets(&self, cwd: &Path, outside: Reason, targets: &[&Word]) -> Option<Reason> {
        if self.partial {
            return Some(Reason::Unsure);
        }
        targets
            .iter()
            .find_map(|target| check_word(target, self.context, cwd, outside))
    }

    // A `cd` that fails leaves the folder as it was, so one into a folder that isn't there
    // can't be followed.
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

    fn copy(&self, cwd: &Path) -> Option<Reason> {
        let symbolic = self.args.iter().any(|arg| {
            arg.text == "--symbolic"
                || short_flags(&arg.text).is_some_and(|flags| flags.contains('s'))
        });
        if self.name == "ln" && symbolic {
            // A link can lead anywhere once it exists.
            return Some(Reason::Unsure);
        }
        let into = value_of(self.args, &["-t", "--target-directory"]);
        let named = positional(self.args);
        let destination = into.as_ref().or_else(|| named.last().copied());
        // A hard link is the file it links to, so writing it later writes that file.
        let linked = self.name == "ln"
            || self.args.iter().any(|arg| {
                arg.text == "--link"
                    || short_flags(&arg.text).is_some_and(|flags| flags.contains('l'))
            });
        let sources: Vec<&Word> = named
            .iter()
            .copied()
            .filter(|word| Some(*word) != destination)
            .collect();
        let inside = destination
            .map(|folder| self.landing(folder, &sources, cwd))
            .unwrap_or_default();
        let linked_sources = sources.iter().copied().filter(|_| linked);
        let targets: Vec<&Word> = destination
            .into_iter()
            .chain(inside.iter())
            .chain(linked_sources)
            .collect();
        self.targets(cwd, Reason::WriteOutside, &targets)
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

    fn dd(&self, cwd: &Path) -> Option<Reason> {
        self.args.iter().find_map(|arg| {
            let file = arg.text.strip_prefix("of=")?;
            check_word(&arg.tail(file), self.context, cwd, Reason::WriteOutside)
        })
    }

    fn find(&self, cwd: &Path) -> Option<Reason> {
        let named = |names: &[&str]| {
            self.args
                .iter()
                .any(|arg| names.contains(&arg.text.as_str()))
        };
        if named(&FIND_RUNS) || named(&FIND_WRITES) {
            return Some(Reason::Unsure);
        }
        if !named(&["-delete"]) {
            return None;
        }
        // Following symlinks, it can reach and delete what lies outside.
        if named(&["-L", "-follow"]) {
            return Some(Reason::Unsure);
        }
        // Options about links and debugging come before the folders to start from.
        let mut rest = self.args;
        while let Some((first, after)) = rest.split_first() {
            let text = first.text.as_str();
            rest = match text {
                "-H" | "-L" | "-P" | "--" => after,
                "-D" => after.get(1..).unwrap_or_default(),
                _ if text.starts_with("-O") => after,
                _ => break,
            };
        }
        let starts: Vec<&Word> = rest
            .iter()
            .take_while(|arg| !arg.text.starts_with(['-', '(', '!']))
            .collect();
        let here = Word::default();
        let starts = if starts.is_empty() {
            vec![&here]
        } else {
            starts
        };
        self.targets(cwd, Reason::DeleteOutside, &starts)
    }
}
