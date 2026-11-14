//! The commands known to be safe: the only ones that run without asking. Anything not here
//! asks, and so does a listed command with an option or a path outside what its entry allows.

use std::path::Path;

use super::shell::Word;
use super::words::{has, positional, short_flags};
use super::{Context, Reason, check_word};

/// Programs that only read. `ANY` marks one with no option that writes, runs, or reads a file
/// it isn't handed as an operand, so any option is safe; the rest list the exact options they
/// may take, and any other option asks. Operands are read-checked against the credential and
/// Weacon folders before this.
const READERS: [(&str, Option<Options>); 33] = [
    ("ls", ANY),
    ("cat", ANY),
    ("head", ANY),
    ("tail", ANY),
    ("wc", ANY),
    ("pwd", ANY),
    ("echo", ANY),
    ("printf", ANY),
    ("true", ANY),
    ("false", ANY),
    ("test", ANY),
    ("[", ANY),
    ("stat", ANY),
    ("which", ANY),
    ("basename", ANY),
    ("dirname", ANY),
    ("realpath", ANY),
    ("readlink", ANY),
    ("uname", ANY),
    ("whoami", ANY),
    ("printenv", ANY),
    ("cut", ANY),
    ("tr", ANY),
    ("du", ANY),
    // `-s`/`--set` would set the clock.
    (
        "date",
        options(
            "udrRI",
            &["--utc", "--date", "--rfc-3339", "--iso-8601", "--reference"],
        ),
    ),
    ("grep", GREP),
    ("egrep", GREP),
    ("fgrep", GREP),
    // `-L`/`--follow` would leave the worktree through a symlink; `--pre*` runs a program.
    (
        "rg",
        options(
            "nilcwxeEsoHApC",
            &[
                "--line-number",
                "--ignore-case",
                "--count",
                "--word-regexp",
                "--regexp",
                "--fixed-strings",
                "--color",
                "--glob",
                "--type",
                "--context",
                "--before-context",
                "--after-context",
                "--hidden",
                "--no-ignore",
                "--files",
                "--smart-case",
                "--multiline",
            ],
        ),
    ),
    (
        "diff",
        options(
            "uUyqrbBiwNpcd",
            &[
                "--unified",
                "--recursive",
                "--brief",
                "--ignore-case",
                "--ignore-all-space",
                "--ignore-space-change",
                "--new-file",
                "--color",
                "--context",
                "--side-by-side",
            ],
        ),
    ),
    // `-o`/`-T`/`--compress-program`/`--files0-from` would write a file or run a program.
    (
        "sort",
        options(
            "nrubfkSsc",
            &[
                "--numeric-sort",
                "--reverse",
                "--unique",
                "--ignore-leading-blanks",
                "--dictionary-order",
                "--key",
                "--field-separator",
                "--stable",
                "--check",
                "--human-numeric-sort",
                "--version-sort",
                "--month-sort",
            ],
        ),
    ),
    // A second operand is the file it writes.
    (
        "uniq",
        Some(Options {
            short: "cduiwfs",
            long: &[
                "--count",
                "--repeated",
                "--unique",
                "--ignore-case",
                "--check-chars",
                "--skip-fields",
                "--skip-chars",
            ],
            operands: 1,
            text: &[],
        }),
    ),
    // `-L`/`-f` read a file, and `import`/`include` in the filter program reads a module.
    (
        "jq",
        Some(Options {
            short: "nrceSMas",
            long: &[
                "--null-input",
                "--raw-output",
                "--compact-output",
                "--slurp",
                "--arg",
                "--argjson",
                "--args",
                "--raw-input",
                "--tab",
                "--sort-keys",
                "--join-output",
                "--exit-status",
            ],
            operands: usize::MAX,
            text: &["import", "include"],
        }),
    ),
];

// `find`'s own options are single words after a single dash, so they are matched whole.
const FIND_FORBIDS: [&str; 9] = [
    "-exec",
    "-execdir",
    "-ok",
    "-okdir",
    "-fprint",
    "-fprint0",
    "-fprintf",
    "-fls",
    "-files0-from",
];
// Options that follow symlinks, so `-delete` could reach past the folders it starts from.
const FIND_FOLLOWS: [&str; 2] = ["-L", "-follow"];

/// Programs that change files, each with the options it may take; every word that isn't an
/// option must name a path inside the chat's worktree.
const WRITERS: [(&str, &str, Reason); 9] = [
    ("mkdir", "pv", Reason::WriteOutside),
    ("touch", "acm", Reason::WriteOutside),
    ("rm", "rRfvd", Reason::DeleteOutside),
    ("rmdir", "pv", Reason::DeleteOutside),
    ("cp", "rRafvpn", Reason::WriteOutside),
    ("mv", "fvn", Reason::DeleteOutside),
    ("ln", "fv", Reason::WriteOutside),
    ("chmod", "Rv", Reason::WriteOutside),
    ("tee", "a", Reason::WriteOutside),
];

/// Build and test tools, by their leading words. What they run, a project's own build scripts
/// and tests, isn't inspected: taking this group out makes every one of them ask.
const BUILD_AND_TEST: [&[&str]; 30] = [
    &["cargo", "build"],
    &["cargo", "check"],
    &["cargo", "test"],
    &["cargo", "clippy"],
    &["cargo", "fmt"],
    &["cargo", "run"],
    &["cargo", "doc"],
    &["cargo", "bench"],
    &["cargo", "nextest"],
    &["cargo", "tree"],
    &["cargo", "metadata"],
    &["npm", "test"],
    &["npm", "run"],
    &["pnpm", "test"],
    &["pnpm", "run"],
    &["yarn", "test"],
    &["yarn", "run"],
    &["make"],
    &["go", "build"],
    &["go", "test"],
    &["go", "vet"],
    &["go", "run"],
    &["pytest"],
    &["python", "-m", "pytest"],
    &["python3", "-m", "pytest"],
    &["tsc"],
    &["eslint"],
    &["prettier"],
    &["deno", "test"],
    &["bun", "test"],
];

/// The git subcommands that run without asking, once git's own checks pass; `git.rs` asks
/// about every other.
pub(super) const GIT_SAFE: [&str; 26] = [
    "status",
    "diff",
    "log",
    "show",
    "add",
    "commit",
    "push",
    "pull",
    "fetch",
    "branch",
    "checkout",
    "switch",
    "restore",
    "stash",
    "rev-parse",
    "ls-files",
    "remote",
    "blame",
    "grep",
    "merge-base",
    "reset",
    "clean",
    "merge",
    "cherry-pick",
    "revert",
    "tag",
];
// Git options that run a command or write a file of their own.
// `--no-index` reads files outside any repository.
pub(super) const GIT_FORBIDS: [&str; 7] = [
    "--receive-pack",
    "--upload-pack",
    "--exec",
    "--output",
    "--ext-diff",
    "--open-files-in-pager",
    "--no-index",
];

/// A reader's allowed options: the short-option letters and the exact long options it may take,
/// the most operands it may be given before one is a file it writes, and operand text that would
/// read or run something. An option outside these asks.
#[derive(Clone, Copy)]
struct Options {
    short: &'static str,
    long: &'static [&'static str],
    operands: usize,
    text: &'static [&'static str],
}

// Any option is safe.
const ANY: Option<Options> = None;

// A reader whose only dangerous options are spelled out; its operands and text are unrestricted.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the table holds Option<Options>, so this builds the Some"
)]
const fn options(short: &'static str, long: &'static [&'static str]) -> Option<Options> {
    Some(Options {
        short,
        long,
        operands: usize::MAX,
        text: &[],
    })
}

// `-R`/`--dereference-recursive` follows symlinks out; plain `-r`/`--recursive` stays put.
const GREP: Option<Options> = options(
    "nricvlLoswxeEFHhq",
    &[
        "--line-number",
        "--recursive",
        "--ignore-case",
        "--invert-match",
        "--count",
        "--files-with-matches",
        "--files-without-match",
        "--only-matching",
        "--word-regexp",
        "--extended-regexp",
        "--fixed-strings",
        "--color",
        "--regexp",
        "--before-context",
        "--after-context",
        "--context",
        "--directories",
        "--include",
        "--exclude",
    ],
);

/// Why a person must be asked before `name` with `args`, run in `cwd`, or nothing when its
/// table entry allows it. Git's own checks have already run.
pub(super) fn judge(name: &str, args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    if let Some((_, options)) = READERS.iter().find(|(reader, _)| *reader == name) {
        return options.and_then(|options| reader(args, &options));
    }
    if name == "find" {
        return find(args, context, cwd);
    }
    if let Some((_, letters, outside)) = WRITERS.iter().find(|(writer, ..)| *writer == name) {
        return writes(args, letters, *outside, context, cwd);
    }
    if is_build(name, args) {
        return builds(name, args, context, cwd);
    }
    Some(Reason::NotListed)
}

// `find` lists; with `-delete` it deletes below the folders it starts from, which must then be
// inside the worktree.
fn find(args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    let named = |names: &[&str]| args.iter().any(|arg| names.contains(&arg.text.as_str()));
    let deletes = named(&["-delete"]);
    if named(&FIND_FORBIDS) || (deletes && named(&FIND_FOLLOWS)) {
        return Some(Reason::NotListed);
    }
    if !deletes {
        return None;
    }
    // Options about links and debugging come before the folders to start from.
    let mut rest = args;
    while let Some((first, after)) = rest.split_first() {
        let text = first.text.as_str();
        rest = match text {
            "-H" | "-P" | "--" => after,
            "-D" => after.get(1..).unwrap_or_default(),
            _ if text.starts_with("-O") => after,
            _ => break,
        };
    }
    let starts = rest
        .iter()
        .take_while(|arg| !arg.text.starts_with(['-', '(', '!']));
    let mut starts = starts.peekable();
    if starts.peek().is_none() {
        return check_word(&Word::default(), context, cwd, Reason::DeleteOutside);
    }
    starts.find_map(|start| check_word(start, context, cwd, Reason::DeleteOutside))
}

// A reader runs only when every option is one it allows, it has no extra operand that it would
// write, and no operand reads or runs something.
fn reader(args: &[Word], options: &Options) -> Option<Reason> {
    if positional(args).len() > options.operands {
        return Some(Reason::NotListed);
    }
    let mut operands = false;
    for arg in args {
        if arg.text == "--" {
            operands = true;
        } else if !operands && arg.text.starts_with('-') && arg.text != "-" {
            if !allowed(&arg.text, options) {
                return Some(Reason::NotListed);
            }
        } else if options.text.iter().any(|text| arg.text.contains(text)) {
            return Some(Reason::NotListed);
        }
    }
    None
}

// Whether `option` is one the reader allows: a long option spelled exactly, or a short cluster
// whose letters are all allowed (digits and punctuation in it are counts or arguments).
fn allowed(option: &str, options: &Options) -> bool {
    if let Some(long) = option.strip_prefix("--") {
        let name = long.split_once('=').map_or(long, |(name, _)| name);
        return options.long.contains(&format!("--{name}").as_str());
    }
    short_flags(option).is_some_and(|flags| {
        flags
            .chars()
            .all(|flag| !flag.is_ascii_alphabetic() || options.short.contains(flag))
    })
}

// Every option one of `letters` and every other word a path inside the worktree.
fn writes(
    args: &[Word],
    letters: &str,
    outside: Reason,
    context: &Context,
    cwd: &Path,
) -> Option<Reason> {
    let mut operands = false;
    for arg in args {
        if arg.text == "--" {
            operands = true;
        } else if !operands && arg.text.starts_with('-') {
            let known = short_flags(&arg.text).is_some_and(|flags| {
                !flags.is_empty() && flags.chars().all(|flag| letters.contains(flag))
            });
            if !known {
                return Some(Reason::NotListed);
            }
        } else if let Some(reason) = check_word(arg, context, cwd, outside) {
            return Some(reason);
        }
    }
    None
}

/// Whether `name` with `args` is one of the build and test tools.
pub(super) fn is_build(name: &str, args: &[Word]) -> bool {
    BUILD_AND_TEST.iter().any(|entry| leads(name, args, entry))
}

// The entry's words lead the command exactly, so no option comes before them.
fn leads(name: &str, args: &[Word], entry: &[&str]) -> bool {
    let Some((program, rest)) = entry.split_first() else {
        return false;
    };
    *program == name
        && rest.len() <= args.len()
        && rest.iter().zip(args).all(|(want, arg)| *want == arg.text)
}

// A build tool may take any option, but no path it is given may lead outside the worktree.
fn builds(name: &str, args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    let runs = args.first().is_some_and(|first| first.text == "run");
    if runs && args.iter().any(|arg| arg.text.starts_with("weacon")) {
        return Some(Reason::Weacon);
    }
    // Text from the line run as a makefile or a cargo setting, or code fetched from elsewhere.
    let inline = match name {
        "make" => has(args, &["--eval"], &['E']),
        "cargo" => has(args, &["--config"], &['Z']),
        _ => false,
    };
    let fetched = args
        .iter()
        .any(|arg| arg.text.contains("://") || (name == "go" && arg.text.contains('@')));
    if inline || fetched {
        return Some(Reason::NotListed);
    }
    args.iter().find_map(|arg| {
        let value = match short_flags(&arg.text) {
            // An attached value, as in `-C../main`, follows the option's letter.
            Some(flags) => arg.tail(flags.get(1..).filter(|value| !value.is_empty())?),
            None if arg.text.starts_with('-') => {
                arg.text.split_once('=').map(|(_, value)| arg.tail(value))?
            }
            None => arg.clone(),
        };
        check_word(&value, context, cwd, Reason::WriteOutside)
    })
}
