//! Reading a simple command's words: which program it runs and with what.

use super::shell::Word;

// Programs that run the command after them; each with its options that take a value, and how
// many words it takes before the command.
const WRAPPERS: [(&str, &[&str], usize); 20] = [
    ("sudo", &["-u", "-g", "-h", "-p", "-r", "-t", "-U"], 0),
    ("doas", &["-u"], 0),
    ("env", &["-u", "--unset"], 0),
    ("command", &[], 0),
    ("builtin", &[], 0),
    ("exec", &["-a"], 0),
    ("nohup", &[], 0),
    ("nice", &["-n"], 0),
    ("time", &["-f", "-o"], 0),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"], 1),
    ("stdbuf", &["-i", "-o", "-e"], 0),
    ("ionice", &["-c", "-n", "-p"], 0),
    ("setsid", &[], 0),
    ("unbuffer", &[], 0),
    ("flock", &["-w", "-E", "--timeout"], 1),
    (
        "xargs",
        &["-I", "-n", "-P", "-L", "-d", "-E", "-s", "-a"],
        0,
    ),
    ("busybox", &[], 0),
    ("coproc", &[], 0),
    ("unshare", &[], 0),
    ("strace", &["-o", "-e", "-p", "-s", "-u"], 0),
];

// Options of the wrappers that change folder or root, or read a command line from text.
const MOVING: [&str; 7] = [
    "-C",
    "--chdir",
    "-D",
    "-S",
    "--split-string",
    "-R",
    "--root",
];

// Programs that run a command given as text, which the guard doesn't follow.
const RUNNERS: [&str; 10] = [
    "su", "runuser", "script", "parallel", "trap", "alias", "chroot", "watch", "ssh", "nsenter",
];

// Words that start or join shell constructs rather than name a program.
const KEYWORDS: [&str; 14] = [
    "{", "}", "!", "(", ")", "if", "then", "else", "elif", "fi", "do", "done", "while", "until",
];

/// The program a simple command runs and its words, or why it can't be told.
pub(super) enum Command<'a> {
    /// Only assignments or keywords.
    Empty,
    /// The program and its words; `partial` when `xargs` adds words of its own.
    Runs {
        words: &'a [Word],
        partial: bool,
    },
    Unsure,
}

/// The program and its words, past assignments, keywords and wrappers.
pub(super) fn command(words: &[Word]) -> Command<'_> {
    let mut rest = words;
    let mut partial = false;
    loop {
        let start = rest
            .iter()
            .position(|word| !is_assignment(word) && !KEYWORDS.contains(&word.text.as_str()));
        let Some(first) = start.and_then(|start| rest.get(start..)) else {
            return Command::Empty;
        };
        rest = first;
        let Some(program) = rest.first() else {
            return Command::Empty;
        };
        let name = program_name(&program.text);
        if RUNNERS.contains(&name.as_str()) {
            return Command::Unsure;
        }
        let Some((_, with_value, before)) = WRAPPERS.iter().find(|(wrapper, ..)| *wrapper == name)
        else {
            return Command::Runs {
                words: rest,
                partial,
            };
        };
        partial |= name == "xargs";
        let after = rest.get(1..).unwrap_or_default();
        let Some((skipped, after)) = skip_options(after, with_value) else {
            return Command::Unsure;
        };
        if skipped.iter().any(|option| moves(&option.text)) {
            return Command::Unsure;
        }
        let Some(after) = after.get(*before..) else {
            return Command::Empty;
        };
        rest = after;
    }
}

fn moves(option: &str) -> bool {
    MOVING
        .iter()
        .any(|moving| option == *moving || option.starts_with(&format!("{moving}=")))
}

// The options at the start of `rest`, and what follows them.
fn skip_options<'a>(rest: &'a [Word], with_value: &[&str]) -> Option<(&'a [Word], &'a [Word])> {
    let mut at = 0;
    while let Some(word) = rest.get(at) {
        if word.text == "--" {
            return Some((rest.get(..at)?, rest.get(at + 1..)?));
        }
        if !word.text.starts_with('-') || word.text == "-" {
            break;
        }
        at += if with_value.contains(&word.text.as_str()) {
            2
        } else {
            1
        };
    }
    Some((
        rest.get(..at.min(rest.len()))?,
        rest.get(at.min(rest.len())..)?,
    ))
}

/// Whether `word` sets a variable, as `NAME=value` does.
pub(super) fn is_assignment(word: &Word) -> bool {
    word.text.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name
                .chars()
                .all(|char| char.is_ascii_alphanumeric() || char == '_')
    })
}

/// The program's file name, lowercase and without `.exe`.
pub(super) fn program_name(text: &str) -> String {
    let base = text
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(text)
        .to_ascii_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base).to_owned()
}

/// The name without a version at its end, so `python3.12` is `python`.
pub(super) fn family(name: &str) -> &str {
    name.trim_end_matches(|char: char| char.is_ascii_digit() || char == '.')
}

/// Whether the command is `pattern`'s program with its words first among its arguments.
pub(super) fn begins(name: &str, args: &[Word], pattern: &[String]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return false;
    };
    let named = positional(args);
    program_name(first) == name
        && rest.len() <= named.len()
        && rest.iter().zip(named).all(|(want, arg)| *want == arg.text)
}

/// Arguments that aren't options; everything after `--` counts.
pub(super) fn positional(args: &[Word]) -> Vec<&Word> {
    let mut after_dashes = false;
    args.iter()
        .filter(|arg| {
            if after_dashes {
                return true;
            }
            if arg.text == "--" {
                after_dashes = true;
                return false;
            }
            !arg.text.starts_with('-')
        })
        .collect()
}

/// The value of the first of `options`, given as the next word or after `=`.
pub(super) fn value_of(args: &[Word], options: &[&str]) -> Option<Word> {
    args.iter().enumerate().find_map(|(at, arg)| {
        options.iter().find_map(|option| {
            if arg.text == *option {
                return args.get(at + 1).cloned();
            }
            let value = arg.text.strip_prefix(&format!("{option}="))?;
            Some(Word {
                text: value.to_owned(),
                ..arg.clone()
            })
        })
    })
}

/// The letters of a cluster of short options, such as `rf` in `-rf`.
pub(super) fn short_flags(text: &str) -> Option<&str> {
    text.strip_prefix('-')
        .filter(|flags| !flags.starts_with('-'))
}

/// Whether `sed` edits its files in place.
pub(super) fn in_place(args: &[Word]) -> bool {
    args.iter().any(|arg| {
        arg.text.starts_with("--in-place")
            || short_flags(&arg.text).is_some_and(|flags| flags.contains('i'))
    })
}

/// The words as one line, when none of them is unknown.
pub(super) fn joined(args: &[Word]) -> Option<String> {
    if args.iter().any(|arg| arg.dynamic) {
        return None;
    }
    Some(
        args.iter()
            .map(|arg| arg.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
}
