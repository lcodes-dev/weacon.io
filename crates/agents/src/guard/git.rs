//! The `git` commands a person must be asked about.

use std::path::{Path, PathBuf};

use super::shell::Word;
use super::words::short_flags;
use super::{Context, Reason, check_word, path_of};
use crate::access::resolve;

// Options of `git push` that take a value, so the value isn't a remote or a refspec.
const PUSH_VALUES: [&str; 5] = ["-o", "--push-option", "--repo", "--receive-pack", "--exec"];
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

/// What `git` with `args`, run in `cwd`, needs a person for.
pub(super) fn judge(args: &[Word], context: &Context, cwd: &Path) -> Option<Reason> {
    let mut repo = cwd.to_path_buf();
    let mut rest = args;
    loop {
        let (first, after) = rest.split_first()?;
        let text = first.text.as_str();
        if text == "-C" {
            let (folder, after) = after.split_first()?;
            let Some(folder) = path_of(folder, context, &repo) else {
                return Some(Reason::Unsure);
            };
            repo = folder;
            rest = after;
        } else if ["--git-dir", "--work-tree", "--config-env", "--exec-path"]
            .iter()
            .any(|option| text.starts_with(option))
        {
            return Some(Reason::Unsure);
        } else if text == "-c" {
            let (setting, after) = after.split_first()?;
            if !safe_setting(setting) {
                return Some(Reason::Unsure);
            }
            rest = after;
        } else if text.starts_with('-') {
            rest = after;
        } else {
            return subcommand(text, after, context, &repo);
        }
    }
}

fn subcommand(name: &str, args: &[Word], context: &Context, repo: &Path) -> Option<Reason> {
    let has = |long: &[&str], short: &[char]| {
        args.iter().any(|arg| {
            long.iter()
                .any(|option| arg.text == *option || arg.text.starts_with(&format!("{option}=")))
                || short_flags(&arg.text).is_some_and(|flags| flags.contains(short))
        })
    };
    match name {
        "push" => push(args, has),
        "branch" if has(&["--delete", "--force"], &['d', 'D', 'f', 'M', 'C']) => {
            Some(Reason::DeleteBranch)
        }
        "update-ref" if has(&[], &['d']) => Some(Reason::DeleteBranch),
        "checkout" if has(&[], &['B']) => Some(Reason::DeleteBranch),
        "switch" if has(&["--force-create"], &['C']) => Some(Reason::DeleteBranch),
        "reset" if has(&["--hard"], &[]) => main_checkout(repo, context),
        "checkout" if discards(args) => main_checkout(repo, context),
        "restore" => main_checkout(repo, context),
        "stash"
            if args
                .first()
                .is_some_and(|action| ["clear", "drop"].contains(&action.text.as_str())) =>
        {
            main_checkout(repo, context)
        }
        "clean" if !has(&["--dry-run"], &['n']) => {
            (!own_checkout(repo, context)).then_some(Reason::CleanMainCheckout)
        }
        "worktree" => worktree(args, context, repo),
        "config" if writes_config(args) => Some(Reason::WriteOutside),
        _ => None,
    }
}

fn push(args: &[Word], has: impl Fn(&[&str], &[char]) -> bool) -> Option<Reason> {
    if has(&["--delete", "--prune", "--mirror"], &['d']) {
        return Some(Reason::DeleteBranch);
    }
    let forced = ["--force", "--force-with-lease", "--force-if-includes"];
    if has(&forced, &['f']) {
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

fn worktree(args: &[Word], context: &Context, repo: &Path) -> Option<Reason> {
    let (action, rest) = args.split_first()?;
    if action.text != "remove" {
        return None;
    }
    rest.iter()
        .filter(|arg| !arg.text.starts_with('-'))
        .find_map(|folder| check_word(folder, context, repo, Reason::DeleteOutside))
}

// `git checkout -- file` and `git checkout .` throw away changes to files.
fn discards(args: &[Word]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.text.as_str(), "--" | "." | "--force") || arg.dynamic)
        || has_short(args, 'f')
}

fn has_short(args: &[Word], flag: char) -> bool {
    args.iter()
        .any(|arg| short_flags(&arg.text).is_some_and(|flags| flags.contains(flag)))
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

fn writes_config(args: &[Word]) -> bool {
    let reads = [
        "--get",
        "--get-all",
        "--get-regexp",
        "--list",
        "-l",
        "--show-origin",
    ];
    let named: Vec<&Word> = args
        .iter()
        .filter(|arg| !arg.text.starts_with('-'))
        .collect();
    let unsets = args
        .iter()
        .any(|arg| arg.text.starts_with("--unset") || arg.text == "--add");
    !args.iter().any(|arg| reads.contains(&arg.text.as_str()))
        && (unsets
            || named.len() >= 2
            || named
                .first()
                .is_some_and(|name| matches!(name.text.as_str(), "set" | "unset")))
}

// Whether `repo` is in the chat's own worktree, which isn't the main checkout.
fn own_checkout(repo: &Path, context: &Context) -> bool {
    let real = |path: &Path| resolve(path).unwrap_or_else(|| PathBuf::from(path));
    let (repo, worktree) = (real(repo), real(&context.worktree));
    let Some(main) = context.main_checkout.as_deref().map(real) else {
        return false;
    };
    repo.starts_with(&worktree) && worktree != main
}
