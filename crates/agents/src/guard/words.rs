//! Reading a simple command's words: which program it runs and with what.

use super::shell::Word;

// Programs that run the command after them; each with its options that take a value, and how
// many words it takes before the command.
const WRAPPERS: [(&str, &[&str], usize); 16] = [
    (
        "sudo",
        &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"],
        0,
    ),
    ("doas", &["-u", "-C"], 0),
    ("env", &["-u", "-C", "--unset", "--chdir"], 0),
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
];

// Words that start or join shell constructs rather than name a program.
const KEYWORDS: [&str; 14] = [
    "{", "}", "!", "(", ")", "if", "then", "else", "elif", "fi", "do", "done", "while", "until",
];

// The program and its words, past assignments, keywords and wrappers; whether `xargs` adds
// words of its own.
pub(super) fn command(words: &[Word]) -> Option<(&[Word], bool)> {
    let mut rest = words;
    let mut partial = false;
    loop {
        while rest
            .first()
            .is_some_and(|word| is_assignment(word) || KEYWORDS.contains(&word.text.as_str()))
        {
            rest = rest.get(1..)?;
        }
        let name = program_name(&rest.first()?.text);
        let Some((_, with_value, before)) = WRAPPERS.iter().find(|(wrapper, ..)| *wrapper == name)
        else {
            return Some((rest, partial));
        };
        partial |= name == "xargs";
        rest = rest.get(1..)?;
        rest = skip_options(rest, with_value)?;
        rest = rest.get(*before..)?;
    }
}

pub(super) fn skip_options<'a>(mut rest: &'a [Word], with_value: &[&str]) -> Option<&'a [Word]> {
    while let Some(word) = rest.first() {
        if word.text == "--" {
            return rest.get(1..);
        }
        if !word.text.starts_with('-') || word.text == "-" {
            break;
        }
        let skip = if with_value.contains(&word.text.as_str()) {
            2
        } else {
            1
        };
        rest = rest.get(skip..)?;
    }
    Some(rest)
}

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

pub(super) fn program_name(text: &str) -> String {
    let base = text
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(text)
        .to_ascii_lowercase();
    base.strip_suffix(".exe")
        .map_or_else(|| base.clone(), str::to_owned)
}

// Whether the command starts with `pattern`'s words.
pub(super) fn begins(name: &str, args: &[Word], pattern: &[String]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return false;
    };
    program_name(first) == name
        && rest.len() <= args.len()
        && rest.iter().zip(args).all(|(want, arg)| *want == arg.text)
}

// Arguments that aren't options; everything after `--` counts.
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
            !arg.text.starts_with('-') || arg.text == "-"
        })
        .collect()
}

pub(super) fn value_of<'a>(args: &'a [Word], options: &[&str]) -> Option<&'a Word> {
    let at = args
        .iter()
        .position(|arg| options.contains(&arg.text.as_str()))?;
    args.get(at + 1)
}

pub(super) fn short_flags(text: &str) -> Option<&str> {
    text.strip_prefix('-')
        .filter(|flags| !flags.starts_with('-'))
}

pub(super) fn in_place(args: &[Word]) -> bool {
    args.iter().any(|arg| {
        arg.text.starts_with("--in-place")
            || short_flags(&arg.text).is_some_and(|flags| flags.starts_with('i'))
    })
}

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
