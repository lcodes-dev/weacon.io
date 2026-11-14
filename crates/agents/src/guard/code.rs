//! Programs that run code or queries given in their arguments: shells, interpreters and
//! database clients.

use super::Reason;
use super::shell::{Simple, Word};
use super::words::{family, short_flags};

const SHELLS: [&str; 9] = [
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "yash", "fish",
];
// Shells whose code isn't the POSIX shell's, so the guard can't read it.
const OTHER_SHELLS: [&str; 5] = ["pwsh", "powershell", "cmd", "csh", "tcsh"];
const INTERPRETERS: [&str; 12] = [
    "python",
    "node",
    "perl",
    "ruby",
    "deno",
    "bun",
    "php",
    "lua",
    "osascript",
    "rscript",
    "pypy",
    "nodejs",
];
const AWKS: [&str; 4] = ["awk", "gawk", "mawk", "nawk"];
const DATABASES: [&str; 10] = [
    "psql",
    "mysql",
    "mariadb",
    "sqlite",
    "mongo",
    "mongosh",
    "cqlsh",
    "clickhouse-client",
    "duckdb",
    "sqlcmd",
];
// Short options that make an interpreter run text from the command line, and long ones.
const INLINE: [char; 5] = ['c', 'e', 'E', 'p', 'r'];
const INLINE_LONG: [&str; 4] = ["--eval", "--print", "--command", "--exec"];
const DROPS: [&str; 6] = [
    "drop database",
    "drop schema",
    "drop table",
    "drop collection",
    "dropdatabase",
    "truncate ",
];

/// What a program that runs code needs a person for; nothing when it isn't one.
pub(super) fn judge(name: &str, args: &[Word], simple: &Simple) -> Option<Reason> {
    let kind = family(name);
    if SHELLS.contains(&kind) {
        return shell(args, simple);
    }
    if OTHER_SHELLS.contains(&kind) {
        return (!args.is_empty() || simple.fed).then_some(Reason::Unsure);
    }
    if INTERPRETERS.contains(&kind) {
        return interpreter(args, simple);
    }
    if AWKS.contains(&kind) {
        return awk(args);
    }
    database(name, args, simple)
}

// Code given inline, as in `bash -lc '…'`, or through a pipe or a here-document, isn't
// followed: a person sees it first.
fn shell(args: &[Word], simple: &Simple) -> Option<Reason> {
    let inline = args
        .iter()
        .any(|arg| short_flags(&arg.text).is_some_and(|flags| flags.contains('c')));
    (inline || simple.fed).then_some(Reason::Unsure)
}

fn interpreter(args: &[Word], simple: &Simple) -> Option<Reason> {
    let inline = args.iter().any(|arg| {
        short_flags(&arg.text).is_some_and(|flags| flags.contains(INLINE))
            || INLINE_LONG
                .iter()
                .any(|option| arg.text.starts_with(option))
    });
    (inline || simple.fed).then_some(Reason::Unsure)
}

// An awk program can run commands and write files; a plain one only reads.
fn awk(args: &[Word]) -> Option<Reason> {
    let risky = args.iter().any(|arg| {
        arg.dynamic
            || ["-i", "-f", "--include", "--file"].contains(&arg.text.as_str())
            || ["system", "getline", "|", ">"]
                .iter()
                .any(|part| arg.text.contains(part))
    });
    risky.then_some(Reason::Unsure)
}

fn database(name: &str, args: &[Word], simple: &Simple) -> Option<Reason> {
    let words: Vec<String> = args.iter().map(|arg| statement(&arg.text)).collect();
    let says = |action: &[&str]| words.iter().any(|word| action.contains(&word.trim()));
    match name {
        "dropdb" => return Some(Reason::DropDatabase),
        "mysqladmin" => return says(&["drop"]).then_some(Reason::DropDatabase),
        "redis-cli" if says(&["flushall", "flushdb"]) => return Some(Reason::DropDatabase),
        "redis-cli" => return simple.fed.then_some(Reason::Unsure),
        "rails" | "rake" if says(&["db:drop", "db:reset", "db:purge"]) => {
            return Some(Reason::DropDatabase);
        }
        "prisma" if says(&["--force-reset"]) || (says(&["migrate"]) && says(&["reset"])) => {
            return Some(Reason::DropDatabase);
        }
        _ => {}
    }
    if !DATABASES.contains(&family(name)) {
        return None;
    }
    if words
        .iter()
        .any(|word| DROPS.iter().any(|drop| word.contains(drop)))
    {
        return Some(Reason::DropDatabase);
    }
    let unknown = args.iter().any(|arg| arg.dynamic);
    (simple.fed || unknown).then_some(Reason::Unsure)
}

// A statement lowercase, with comments gone and each run of space as one, so spacing can't
// hide what it does.
fn statement(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("/*") {
        plain.push_str(before);
        plain.push(' ');
        rest = after.split_once("*/").map_or("", |(_, after)| after);
    }
    plain.push_str(rest);
    plain
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        + " "
}
