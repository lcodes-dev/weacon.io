//! Reading a simple command's words: which program it runs and with what.

use super::shell::Word;

// Programs that run the command after them and do nothing else, each with its options that
// take a value, and how many words it takes before the command. Any other wrapper isn't on the
// table of what's known safe, so it asks.
const WRAPPERS: [(&str, &[&str], usize); 4] = [
    ("env", &["-u", "--unset"], 0),
    ("nice", &["-n"], 0),
    ("time", &["-f", "-o"], 0),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"], 1),
];

// Wrappers' options that name a file they write.
const WRITES_TO: [(&str, &str); 2] = [("time", "-o"), ("time", "--output")];

// Options of the wrappers that change folder or read a command line from text.
const MOVING: [&str; 4] = ["-C", "--chdir", "-S", "--split-string"];

// Words that start or join shell constructs rather than name a program.
const KEYWORDS: [&str; 14] = [
    "{", "}", "!", "(", ")", "if", "then", "else", "elif", "fi", "do", "done", "while", "until",
];

/// The program a simple command runs and its words, or why it can't be told.
pub(super) enum Command<'a> {
    /// Only assignments or keywords.
    Empty,
    /// The program and its words.
    Runs(&'a [Word]),
    Unsure,
}

/// The program and its words, past assignments, keywords and wrappers.
pub(super) fn command(words: &[Word]) -> Command<'_> {
    let mut rest = words;
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
        // A wildcard in the program's name is expanded to a program the guard can't name.
        let name = program_name(&program.text);
        if program.dynamic || program.glob {
            return Command::Unsure;
        }
        let Some((_, with_value, before)) = WRAPPERS.iter().find(|(wrapper, ..)| *wrapper == name)
        else {
            return Command::Runs(rest);
        };
        let after = rest.get(1..).unwrap_or_default();
        let (skipped, after) = skip_options(after, with_value);
        if skipped
            .iter()
            .any(|option| unclear(&name, &option.text, with_value))
        {
            return Command::Unsure;
        }
        let Some(after) = after.get(*before..) else {
            return Command::Empty;
        };
        rest = after;
    }
}

// Whether a wrapper's option moves the command elsewhere, names a file the wrapper writes, or
// is a long one that may take the next word as its value without the guard knowing.
fn unclear(wrapper: &str, option: &str, with_value: &[&str]) -> bool {
    let unknown =
        option.starts_with("--") && !option.contains('=') && !with_value.contains(&option);
    let writes = WRITES_TO
        .iter()
        .any(|(writer, writing)| *writer == wrapper && option.starts_with(writing));
    moves(option) || unknown || writes
}

// A short option may carry its value or sit among others, as in `-C/tmp` or `-iC`.
fn moves(option: &str) -> bool {
    MOVING.iter().any(|moving| match moving.strip_prefix('-') {
        Some(letter) if !letter.starts_with('-') => {
            short_flags(option).is_some_and(|flags| flags.contains(letter))
        }
        _ => option == *moving || option.starts_with(&format!("{moving}=")),
    })
}

// The options at the start of `rest`, and what follows them.
fn skip_options<'a>(rest: &'a [Word], with_value: &[&str]) -> (&'a [Word], &'a [Word]) {
    let mut at = 0;
    for (index, word) in rest.iter().enumerate() {
        if index < at {
            continue;
        }
        if word.text == "--" {
            let (options, after) = rest.split_at_checked(index).unwrap_or((rest, &[]));
            return (options, after.get(1..).unwrap_or_default());
        }
        if !word.text.starts_with('-') || word.text == "-" {
            break;
        }
        at = index
            + if with_value.contains(&word.text.as_str()) {
                2
            } else {
                1
            };
    }
    rest.split_at_checked(at).unwrap_or((rest, &[]))
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

/// Whether the command is `pattern`'s program with its words, in order, among its arguments;
/// an option's value may come between them.
pub(super) fn begins(name: &str, args: &[Word], pattern: &[String]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return false;
    };
    let mut named = positional(args).into_iter();
    program_name(first) == name && rest.iter().all(|want| named.any(|arg| arg.text == *want))
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
    values_of(args, options).into_iter().next()
}

/// The values of every one of `options`, in order.
pub(super) fn values_of(args: &[Word], options: &[&str]) -> Vec<Word> {
    // Past `--`, words are operands, not options.
    let options_end = args
        .iter()
        .position(|arg| arg.text == "--")
        .unwrap_or(args.len());
    args.iter()
        .take(options_end)
        .enumerate()
        .filter_map(|(at, arg)| {
            options.iter().find_map(|option| {
                if arg.text == *option {
                    return args.get(at + 1).cloned();
                }
                // A short option's value may follow it at once, as in `-t/etc`.
                let value = arg.text.strip_prefix(&format!("{option}=")).or_else(|| {
                    (option.len() == 2)
                        .then(|| arg.text.strip_prefix(option))
                        .flatten()
                })?;
                Some(arg.tail(value))
            })
        })
        .collect()
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
