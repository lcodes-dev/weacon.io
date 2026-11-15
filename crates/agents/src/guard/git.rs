//! The `git` commands a person must be asked about.

use std::path::{Path, PathBuf};

use super::safe::{GIT_FORBIDS, GIT_SAFE};
use super::shell::Word;
use super::words::{has, positional};
use super::{Context, Reason, path_of};
use crate::access::resolve;

// Options of `git push` that take a value, so the value isn't a remote or a refspec.
const PUSH_VALUES: [&str; 3] = ["-o", "--push-option", "--repo"];
// Settings `-c` may give that run no command and reach no other repository; others can.
const SAFE_SETTINGS: [&str; 9] = [
    "user.",
    "color.",
    "advice.",
    "core.quotepath",
    "init.defaultbranch",
    "commit.gpgsign",
    "pull.rebase",
    "log.",
    "diff.renames",
];
// Global options that take no value; any other could hide the subcommand.
const PLAIN: [&str; 12] = [
    "-p",
    "-P",
    "--paginate",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-optional-locks",
    "--no-advice",
];
// Subcommands that, in the main checkout, can throw away its changes or files.
const DISCARDING: [&str; 4] = ["checkout", "switch", "restore", "stash"];
// What `stash` does that keeps every stash; the stashes are shared by every worktree.
const STASH_KEEPS: [&str; 4] = ["list", "show", "push", "save"];
// Subcommands that change neither a checkout's files nor its branch, so they may look anywhere.
const LOOKS: [&str; 12] = [
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "ls-files",
    "remote",
    "blame",
    "grep",
    "merge-base",
    "branch",
    "fetch",
];
// What `remote` does that changes no setting.
const REMOTE_LOOKS: [&str; 4] = ["-v", "--verbose", "show", "get-url"];

// Subcommands that rewrite the working tree, so a path a later command in the same line resolved
// may no longer be what is there.
const TREE_REWRITERS: [&str; 11] = [
    "checkout",
    "switch",
    "restore",
    "reset",
    "merge",
    "pull",
    "rebase",
    "cherry-pick",
    "revert",
    "stash",
    "clean",
];

/// Whether `git` with `args` rewrites the working tree, read past git's own global options.
pub(super) fn rewrites_tree(args: &[Word]) -> bool {
    let mut rest = args;
    loop {
        let Some((first, after)) = rest.split_first() else {
            return false;
        };
        let text = first.text.as_str();
        if text == "-C" || text == "-c" {
            rest = after.get(1..).unwrap_or_default();
        } else if PLAIN.contains(&text) {
            rest = after;
        } else if text.starts_with('-') {
            return false;
        } else {
            return TREE_REWRITERS.contains(&text);
        }
    }
}

/// What `git` with `args`, run in `cwd`, needs a person for.
pub(super) fn judge(args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    let mut repo = cwd.to_path_buf();
    let mut rest = args;
    loop {
        let (first, after) = rest.split_first()?;
        if first.dynamic || first.glob {
            return Some(Reason::Unsure);
        }
        let text = first.text.as_str();
        if text == "-C" {
            let (folder, after) = after.split_first()?;
            let Some(folder) = path_of(folder, context, &repo) else {
                return Some(Reason::Unsure);
            };
            repo = folder;
            rest = after;
        } else if text == "-c" {
            let (setting, after) = after.split_first()?;
            if !safe_setting(setting) {
                return Some(Reason::Unsure);
            }
            rest = after;
        } else if PLAIN.contains(&text) {
            rest = after;
        } else if text.starts_with('-') {
            return Some(Reason::Unsure);
        } else {
            return subcommand(text, after, context, &repo);
        }
    }
}

fn subcommand(name: &str, args: &[Word], context: &Context, repo: &Path) -> Option<Reason> {
    let has = |long: &[&str], short: &[char]| has(args, long, short);
    // git permutes options among the operands, so a dynamic or glob word anywhere could be any
    // option — a pager that runs a program, an output path outside the worktree — so unless it is
    // past a literal `--` or the value of commit's message, the run can't be proven safe.
    if hidden_option(name, args) {
        return Some(Reason::Unsure);
    }
    // A subcommand's action is its first non-option word before any `--`: `remote -v set-url`
    // acts on `set-url`, while `stash -- show` has no action (an implicit push of pathspec `show`).
    let first = args
        .iter()
        .take_while(|arg| arg.text != "--")
        .map(|arg| arg.text.as_str())
        .find(|text| !text.starts_with('-'));
    let unlisted = match name {
        "grep" => has(&[], &['O']),
        "remote" => first.is_some_and(|action| !REMOTE_LOOKS.contains(&action)),
        "stash" => first.is_some_and(|action| !STASH_KEEPS.contains(&action)),
        _ => !GIT_SAFE.contains(&name),
    };
    if unlisted || has(&GIT_FORBIDS, &[]) {
        return Some(Reason::NotListed);
    }
    // Elsewhere, as in the main checkout, only what changes nothing runs without asking.
    let elsewhere = !LOOKS.contains(&name) && !inside_worktree(repo, context);
    match name {
        "push" => push(args).or_else(|| elsewhere.then_some(Reason::WriteOutside)),
        "branch"
            if has(
                &["--delete", "--force", "--move"],
                &['d', 'D', 'f', 'm', 'M', 'C'],
            ) =>
        {
            Some(Reason::DeleteBranch)
        }
        "tag" if has(&["--delete", "--force"], &['d', 'f']) => Some(Reason::DeleteBranch),
        "fetch" | "pull" if fetch_overwrites(args) => Some(Reason::DeleteBranch),
        "checkout" if has(&[], &['B']) => Some(Reason::DeleteBranch),
        "switch" if has(&["--force-create"], &['C']) => Some(Reason::DeleteBranch),
        "reset" if has(&["--hard", "--merge", "--keep"], &[]) => main_checkout(repo, context),
        // A new branch, or a stash, keeps the checkout's changes.
        "stash" if matches!(first, Some("list" | "show")) => None,
        "stash" => elsewhere.then_some(Reason::WriteOutside),
        "checkout" if has(&[], &['b']) && !has(&["--force"], &['f']) => {
            elsewhere.then_some(Reason::WriteOutside)
        }
        "switch"
            if has(&["--create"], &['c']) && !has(&["--discard-changes", "--force"], &['f']) =>
        {
            elsewhere.then_some(Reason::WriteOutside)
        }
        _ if DISCARDING.contains(&name) => main_checkout(repo, context),
        // Outside the chat's own worktree a clean could delete the main checkout's files; a `-n`
        // dry run can't be told from the real thing once the shell has had the line, so it asks.
        "clean" => (!own_checkout(repo, context)).then_some(Reason::CleanMainCheckout),
        _ => elsewhere.then_some(Reason::WriteOutside),
    }
}

// Whether a word the shell alone can spell, or a glob, could be an option here. Only a word after
// a literal `--`, and the value of commit's `-m`/`--message`, cannot be one.
fn hidden_option(name: &str, args: &[Word]) -> bool {
    let mut operands = false;
    let mut message = false;
    for arg in args {
        if std::mem::take(&mut message) {
            continue;
        }
        // Only a literal `--` ends the options; a dynamic `--$x` has known text `--` but could
        // expand to anything, so it is judged as the hidden option it may be.
        if !arg.dynamic && !arg.glob && arg.text == "--" {
            operands = true;
            continue;
        }
        if operands {
            continue;
        }
        if name == "commit" {
            // `-m X` takes the next word as the message only when `-m` is a plain literal word; a
            // dynamic or glob `-m"$A"` / `--message=$A` carries the message in this word itself, so
            // skip only this one and leave the next to be judged.
            if !arg.dynamic && !arg.glob && matches!(arg.text.as_str(), "-m" | "--message") {
                message = true;
                continue;
            }
            if arg.text.starts_with("-m") || arg.text.starts_with("--message=") {
                continue;
            }
        }
        if arg.dynamic || arg.glob {
            return true;
        }
    }
    false
}

// A forced refspec with a local side, as in `+main:main`, overwrites that branch.
fn fetch_overwrites(args: &[Word]) -> bool {
    let forced = has(args, &["--force"], &['f']);
    positional(args).iter().any(|refspec| {
        refspec.dynamic || (refspec.text.contains(':') && (forced || refspec.text.starts_with('+')))
    })
}

fn push(args: &[Word]) -> Option<Reason> {
    if has(args, &["--delete", "--prune", "--mirror"], &['d']) {
        return Some(Reason::DeleteBranch);
    }
    let forced = ["--force", "--force-with-lease", "--force-if-includes"];
    if has(args, &forced, &['f']) {
        return Some(Reason::ForcePush);
    }
    let mut refspecs = Vec::new();
    let mut skip = false;
    for arg in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        if PUSH_VALUES.contains(&arg.text.as_str()) {
            skip = true;
        } else if !arg.text.starts_with('-') {
            refspecs.push(arg);
        }
    }
    // The first is the remote.
    let refspecs = refspecs.get(1..).unwrap_or_default();
    if refspecs.iter().any(|refspec| refspec.dynamic) {
        return Some(Reason::Unsure);
    }
    if refspecs.iter().any(|refspec| refspec.text.starts_with(':')) {
        return Some(Reason::DeleteBranch);
    }
    refspecs
        .iter()
        .any(|refspec| refspec.text.starts_with('+'))
        .then_some(Reason::ForcePush)
}

fn safe_setting(setting: &Word) -> bool {
    let key = setting
        .text
        .split_once('=')
        .map_or(setting.text.as_str(), |(key, _)| key);
    let key = key.to_ascii_lowercase();
    !setting.dynamic
        && SAFE_SETTINGS
            .iter()
            .any(|safe| key.starts_with(safe) || key == safe.trim_end_matches('.'))
}

fn main_checkout(repo: &Path, context: &Context) -> Option<Reason> {
    (!own_checkout(repo, context)).then_some(Reason::ResetMainCheckout)
}

// Whether `repo` is in the chat's own worktree, which isn't the main checkout.
fn own_checkout(repo: &Path, context: &Context) -> bool {
    let Some(main) = context.main_checkout.as_deref().map(real) else {
        return false;
    };
    inside_worktree(repo, context) && real(&context.worktree) != main
}

fn inside_worktree(repo: &Path, context: &Context) -> bool {
    // When `repo` can't be resolved — a `..` the file system couldn't follow, or a dangling
    // symlink — where it lands isn't known, so don't count it as inside the worktree.
    resolve(repo).is_some_and(|repo| repo.starts_with(real(&context.worktree)))
}

fn real(path: &Path) -> PathBuf {
    resolve(path).unwrap_or_else(|| PathBuf::from(path))
}
