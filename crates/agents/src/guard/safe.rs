//! The commands known to be safe: the only ones that run without asking. Anything not here
//! asks, and so does a listed command with an option or a path outside what its entry allows.

use std::path::Path;

use super::shell::Word;
use super::words::{has, positional, short_flags};
use super::{Context, Reason, check_word};

/// Programs that only read, each with the options that would make it write or run something.
/// Their paths may be anywhere but the credential and Weacon folders, which are read-checked
/// before this.
const READERS: [(&str, Forbids); 33] = [
    ("ls", NONE),
    ("cat", NONE),
    ("head", NONE),
    ("tail", NONE),
    ("wc", NONE),
    ("pwd", NONE),
    ("echo", NONE),
    ("printf", NONE),
    ("true", NONE),
    ("false", NONE),
    ("test", NONE),
    ("[", NONE),
    ("stat", NONE),
    ("which", NONE),
    ("basename", NONE),
    ("dirname", NONE),
    ("realpath", NONE),
    ("readlink", NONE),
    (
        "date",
        Forbids {
            short: &['s'],
            long: &["--set"],
            operands: usize::MAX,
        },
    ),
    ("uname", NONE),
    ("whoami", NONE),
    ("printenv", NONE),
    ("grep", NONE),
    ("egrep", NONE),
    ("fgrep", NONE),
    (
        "rg",
        Forbids {
            short: &[],
            long: &["--pre", "--pre-glob"],
            operands: usize::MAX,
        },
    ),
    (
        "diff",
        Forbids {
            short: &[],
            long: &["--to-file", "--from-file"],
            operands: usize::MAX,
        },
    ),
    ("cut", NONE),
    ("tr", NONE),
    (
        "sort",
        Forbids {
            short: &['o', 'T'],
            long: &["--output", "--temporary-directory"],
            operands: usize::MAX,
        },
    ),
    // A second operand is the file it writes.
    (
        "uniq",
        Forbids {
            short: &[],
            long: &[],
            operands: 1,
        },
    ),
    ("du", NONE),
    ("jq", NONE),
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
pub(super) const GIT_FORBIDS: [&str; 5] = [
    "--receive-pack",
    "--upload-pack",
    "--exec",
    "--output",
    "--ext-diff",
];

/// Options that make a reader write or run something, and how many operands it may take.
#[derive(Clone, Copy)]
struct Forbids {
    short: &'static [char],
    long: &'static [&'static str],
    operands: usize,
}

const NONE: Forbids = Forbids {
    short: &[],
    long: &[],
    operands: usize::MAX,
};

/// Why a person must be asked before `name` with `args`, run in `cwd`, or nothing when its
/// table entry allows it. Git's own checks have already run.
pub(super) fn judge(name: &str, args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    if let Some((_, forbids)) = READERS.iter().find(|(reader, _)| *reader == name) {
        return writes_or_runs(args, forbids).then_some(Reason::NotListed);
    }
    if name == "find" {
        return find(args, context, cwd);
    }
    if let Some((_, letters, outside)) = WRITERS.iter().find(|(writer, ..)| *writer == name) {
        return writes(args, letters, *outside, context, cwd);
    }
    if BUILD_AND_TEST.iter().any(|entry| leads(name, args, entry)) {
        return builds(args, context, cwd);
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

// Whether a reader's arguments use one of its writing options or too many operands.
fn writes_or_runs(args: &[Word], forbids: &Forbids) -> bool {
    positional(args).len() > forbids.operands || has(args, forbids.long, forbids.short)
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
fn builds(args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    // Running Weacon's own build runs Weacon, and `make --eval` runs text from the line.
    let runs = args.first().is_some_and(|first| first.text == "run");
    if runs && args.iter().any(|arg| arg.text.starts_with("weacon")) {
        return Some(Reason::Weacon);
    }
    if args.iter().any(|arg| arg.text.starts_with("--eval")) {
        return Some(Reason::NotListed);
    }
    args.iter().find_map(|arg| {
        let value = if arg.text.starts_with('-') {
            arg.text.split_once('=').map(|(_, value)| arg.tail(value))?
        } else {
            arg.clone()
        };
        check_word(&value, context, cwd, Reason::WriteOutside)
    })
}
