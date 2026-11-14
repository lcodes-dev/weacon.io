//! What each simple command does that a person must be asked about.

use std::path::{Path, PathBuf};

use super::shell::{Simple, Word};
use super::words::{
    begins, command, in_place, joined, positional, program_name, short_flags, value_of,
};
use super::{Context, Reason, changed, git, path_of, reading};

const DELETERS: [&str; 7] = [
    "rm",
    "rmdir",
    "unlink",
    "shred",
    "srm",
    "trash",
    "trash-put",
];
const COPIERS: [&str; 4] = ["cp", "install", "rsync", "ln"];
const WRITERS: [&str; 9] = [
    "tee", "touch", "mkdir", "truncate", "chmod", "chown", "chgrp", "chattr", "setfacl",
];
const SHELLS: [&str; 8] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "pwsh",
    "powershell",
];
const INTERPRETERS: [&str; 11] = [
    "python",
    "python3",
    "node",
    "perl",
    "ruby",
    "deno",
    "bun",
    "php",
    "lua",
    "osascript",
    "rscript",
];
const DATABASES: [&str; 10] = [
    "psql",
    "mysql",
    "mariadb",
    "sqlite3",
    "mongo",
    "mongosh",
    "cqlsh",
    "clickhouse-client",
    "duckdb",
    "sqlcmd",
];
// Output that goes to no file.
const DEVICES: [&str; 5] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty", "nul"];

/// What `simple`, run in `cwd`, needs a person for; a `cd` moves `cwd` for what follows.
pub(super) fn judge(simple: &Simple, context: &Context, cwd: &mut PathBuf) -> Option<Reason> {
    let written = simple
        .writes
        .iter()
        .filter(|target| !DEVICES.contains(&target.text.to_ascii_lowercase().as_str()))
        .find_map(|target| changed(target, context, cwd, Reason::WriteOutside));
    if written.is_some() {
        return written;
    }
    let (words, partial) = command(&simple.words)?;
    let program = words.first()?;
    if program.dynamic {
        return Some(Reason::Unsure);
    }
    let name = program_name(&program.text);
    let args = words.get(1..).unwrap_or_default();
    if let Some(reason) = credentials(words, context, cwd) {
        return Some(reason);
    }
    let extended = context
        .extensions
        .iter()
        .any(|pattern| begins(&name, args, pattern));
    if extended {
        return Some(Reason::Extension);
    }
    let call = Call {
        name: &name,
        args,
        simple,
        context,
        partial,
    };
    call.judge(cwd)
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
            "git" => git::judge(self.args, self.context, cwd),
            "mv" => self.targets(cwd, Reason::DeleteOutside, &positional(self.args)),
            "dd" => self.dd(cwd),
            "find" => self.find(cwd),
            "eval" => {
                joined(self.args).map_or(Some(Reason::Unsure), |line| self.nested(&line, cwd))
            }
            "dropdb" => Some(Reason::DropDatabase),
            _ if DELETERS.contains(&name) => {
                self.targets(cwd, Reason::DeleteOutside, &positional(self.args))
            }
            _ if COPIERS.contains(&name) => self.copy(cwd),
            _ if WRITERS.contains(&name) || (name == "sed" && in_place(self.args)) => {
                self.targets(cwd, Reason::WriteOutside, &positional(self.args))
            }
            _ if SHELLS.contains(&name) => self.shell(cwd),
            _ if INTERPRETERS.contains(&name) => self.interpreter(),
            _ => self.database(),
        }
    }

    fn targets(&self, cwd: &Path, outside: Reason, targets: &[&Word]) -> Option<Reason> {
        if self.partial {
            return Some(Reason::Unsure);
        }
        targets
            .iter()
            .find_map(|target| changed(target, self.context, cwd, outside))
    }

    fn change_folder(&self, cwd: &mut PathBuf) -> Option<Reason> {
        let target = positional(self.args);
        let folder = match target.first() {
            Some(word) => path_of(word, self.context, cwd),
            None => self.context.home.clone(),
        };
        let Some(folder) = folder else {
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
            .or_else(|| positional(self.args).last().copied());
        destination.and_then(|word| self.targets(cwd, Reason::WriteOutside, &[word]))
    }

    fn dd(&self, cwd: &Path) -> Option<Reason> {
        self.args.iter().find_map(|arg| {
            let file = arg.text.strip_prefix("of=")?;
            let target = Word {
                text: file.to_owned(),
                ..arg.clone()
            };
            changed(&target, self.context, cwd, Reason::WriteOutside)
        })
    }

    fn find(&self, cwd: &Path) -> Option<Reason> {
        let runs = ["-exec", "-execdir", "-ok", "-okdir"];
        if self
            .args
            .iter()
            .any(|arg| runs.contains(&arg.text.as_str()))
        {
            return Some(Reason::Unsure);
        }
        if !self.args.iter().any(|arg| arg.text == "-delete") {
            return None;
        }
        let starts: Vec<&Word> = self
            .args
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

    fn shell(&self, cwd: &Path) -> Option<Reason> {
        if let Some(code) = value_of(self.args, &["-c", "-Command", "-command"]) {
            return if code.dynamic {
                Some(Reason::Unsure)
            } else {
                self.nested(&code.text, cwd)
            };
        }
        // Commands from a pipe or a here-document aren't in the command line.
        self.simple.fed.then_some(Reason::Unsure)
    }

    fn interpreter(&self) -> Option<Reason> {
        let inline = ["-c", "-e", "-E", "--eval", "-p", "--print", "-r"];
        let runs_text = self
            .args
            .iter()
            .any(|arg| inline.contains(&arg.text.as_str()));
        (runs_text || self.simple.fed).then_some(Reason::Unsure)
    }

    fn database(&self) -> Option<Reason> {
        let lower = |word: &Word| word.text.to_ascii_lowercase();
        let dropping = |word: &Word| {
            let text = lower(word);
            [
                "drop database",
                "drop schema",
                "drop table",
                "dropdatabase",
                "truncate ",
            ]
            .iter()
            .any(|statement| text.contains(statement))
        };
        let words: Vec<String> = self.args.iter().map(lower).collect();
        let named = |name: &str, action: &[&str]| {
            self.name == name && words.iter().any(|word| action.contains(&word.as_str()))
        };
        if named("mysqladmin", &["drop"]) || named("redis-cli", &["flushall", "flushdb"]) {
            return Some(Reason::DropDatabase);
        }
        if !DATABASES.contains(&self.name) {
            return None;
        }
        if self.args.iter().any(dropping) {
            return Some(Reason::DropDatabase);
        }
        let dynamic = self.args.iter().any(|arg| arg.dynamic);
        (self.simple.fed || dynamic).then_some(Reason::Unsure)
    }

    fn nested(&self, line: &str, cwd: &Path) -> Option<Reason> {
        let inner = Context {
            cwd: cwd.to_path_buf(),
            ..self.context.clone()
        };
        super::check_command(line, &inner)
    }
}

// Asks about any word that names a credential folder or file.
fn credentials(words: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    words
        .iter()
        .filter_map(|word| path_of(word, context, cwd))
        .find_map(|path| reading(&path, context))
}
