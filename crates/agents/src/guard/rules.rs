//! What each simple command does that a person must be asked about.

use std::path::{Path, PathBuf};

use super::shell::{Simple, Word};
use super::words::{
    Command, begins, command, in_place, is_assignment, positional, program_name, short_flags,
    value_of,
};
use super::{Context, Reason, check_word, code, git, path_of, reading};

const DELETERS: [&str; 8] = [
    "rm",
    "rmdir",
    "unlink",
    "shred",
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
const WRITERS: [&str; 9] = [
    "tee", "touch", "mkdir", "truncate", "chmod", "chown", "chgrp", "chattr", "setfacl",
];
// Output that goes to no file.
const DEVICES: [&str; 5] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty", "nul"];
// Of the variables that point git at another repository or have it run a command, these do
// neither.
const GIT_HARMLESS: [&str; 2] = ["GIT_PAGER", "GIT_TERMINAL_PROMPT"];
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
                .find_map(|source| read(source, context, cwd, false))
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
    let reads = (!matches!(name.as_str(), "cd" | "pushd"))
        .then(|| words.iter().find_map(|word| read(word, context, cwd, deep)))
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
            if name.starts_with("GIT_") && !GIT_HARMLESS.contains(&name) {
                return Some(Reason::Unsure);
            }
            read(word, context, cwd, false)
        })
}

// A word may name a file itself, or as an option's value, as in `--file=…` or `-d @…`.
fn read(word: &Word, context: &Context, cwd: &Path, deep: bool) -> Option<Reason> {
    let value = word.text.split_once('=').map(|(_, value)| value);
    let value = value
        .or_else(|| word.text.strip_prefix('@'))
        .filter(|value| !value.is_empty());
    let inner = value.map(|value| Word {
        text: value.to_owned(),
        tilde: value.starts_with('~'),
        ..word.clone()
    });
    inner
        .and_then(|inner| read_path(&inner, context, cwd, deep))
        .or_else(|| read_path(word, context, cwd, deep))
}

// A wildcard can match a credential folder below where it starts, and so can an expansion
// below the folder written before it.
fn read_path(word: &Word, context: &Context, cwd: &Path, deep: bool) -> Option<Reason> {
    if let Some(path) = path_of(word, context, cwd) {
        return reading(&path, context, deep || word.glob);
    }
    if !word.dynamic {
        return Some(Reason::Unsure);
    }
    let (folder, _) = word.text.rsplit_once('/')?;
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
                let output = value_of(self.args, options);
                output.and_then(|word| self.targets(cwd, Reason::WriteOutside, &[&word]))
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
        let destination = value_of(self.args, &["-t", "--target-directory"])
            .or_else(|| positional(self.args).last().copied().cloned());
        destination.and_then(|word| self.targets(cwd, Reason::WriteOutside, &[&word]))
    }

    fn dd(&self, cwd: &Path) -> Option<Reason> {
        self.args.iter().find_map(|arg| {
            let file = arg.text.strip_prefix("of=")?;
            let target = Word {
                text: file.to_owned(),
                ..arg.clone()
            };
            check_word(&target, self.context, cwd, Reason::WriteOutside)
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
