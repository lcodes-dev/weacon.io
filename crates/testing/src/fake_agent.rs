//! A stand-in for Claude Code in the end-to-end tests. It replays the recorded streams in
//! `crates/agents/replays/claude`, answers the sign-in status question with made-up data, and
//! keeps each prompt it was given for the test to read.

use std::io::{self, BufRead, Write};

use serde_json::Value;

use crate::replay::{Step, steps};

const TURN: &str = include_str!("../../agents/replays/claude/turn.replay");
const RECORDINGS: [&str; 3] = [
    TURN,
    include_str!("../../agents/replays/claude/deny.replay"),
    include_str!("../../agents/replays/claude/interrupt.replay"),
];

/// What `claude auth status --json` prints for a claude.ai sign-in.
const SIGNED_IN: &str = "{\"authMethod\":\"claude.ai\"}\n";

/// Answers one call with `args`: the sign-in status, or a stream-json session on `input` and
/// `output`, each of whose prompts' content it writes to `heard` as a line of JSON. False for
/// any other call, which it doesn't answer.
///
/// # Errors
/// When `input`, `output` or `heard` fails.
pub fn answer(
    args: &[String],
    input: impl BufRead,
    mut output: impl Write,
    heard: impl Write,
) -> io::Result<bool> {
    if args == ["auth", "status", "--json"] {
        output.write_all(SIGNED_IN.as_bytes())?;
        Ok(true)
    } else if args.first().is_some_and(|arg| arg == "--print") {
        converse(input, output, heard).map(|()| true)
    } else {
        Ok(false)
    }
}

// For each prompt, plays the recording that starts with it, else `TURN`. Ends with `input`.
fn converse(input: impl BufRead, mut output: impl Write, mut heard: impl Write) -> io::Result<()> {
    let mut lines = input.lines();
    while let Some(prompt) = lines.next().transpose()? {
        if let Some(content) = content(&prompt) {
            writeln!(heard, "{content}")?;
            heard.flush()?;
        }
        play(recording_for(&prompt), &mut lines, &mut output)?;
    }
    Ok(())
}

// Plays `recording` after its prompt: prints the lines it prints, and checks one of `lines`
// against each it reads, until `lines` ends.
fn play(
    recording: &str,
    lines: &mut impl Iterator<Item = io::Result<String>>,
    output: &mut impl Write,
) -> io::Result<()> {
    for step in steps(recording).skip(1) {
        match step.map_err(io::Error::other)? {
            Step::Prints(line) => {
                writeln!(output, "{line}")?;
                output.flush()?;
            }
            Step::Reads(expected) => {
                let Some(sent) = lines.next().transpose()? else {
                    return Ok(());
                };
                if json(&sent) != json(expected) {
                    return Err(io::Error::other(format!(
                        "expected {expected}\nbut got {sent}"
                    )));
                }
            }
        }
    }
    Ok(())
}

// The recording whose prompt the person's own words end with, after a mention of the agent
// and before any quotes; `TURN` when there is none.
fn recording_for(prompt: &str) -> &'static str {
    let asked = content(prompt);
    let asked = asked.as_ref().and_then(Value::as_str).unwrap_or_default();
    let own = asked.split("\n\n---\n").next().unwrap_or_default();
    RECORDINGS
        .into_iter()
        .find(|recording| match steps(recording).next() {
            Some(Ok(Step::Reads(first))) => content(first)
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|recorded| own.ends_with(recorded)),
            _ => false,
        })
        .unwrap_or(TURN)
}

fn content(line: &str) -> Option<Value> {
    json(line)?.pointer("/message/content").cloned()
}

fn json(line: &str) -> Option<Value> {
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Whether it answered, and the lines it printed.
    fn call(args: &[&str], input: &str) -> (bool, Vec<String>) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut output = Vec::new();
        let answered = answer(&args, input.as_bytes(), &mut output, io::sink()).unwrap();
        let printed = String::from_utf8(output).unwrap();
        (answered, printed.lines().map(str::to_owned).collect())
    }

    fn prompt(text: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n")
    }

    // The lines `recording` prints.
    fn printed(recording: &str) -> Vec<String> {
        steps(recording)
            .filter_map(|step| match step.unwrap() {
                Step::Prints(line) => Some(line.to_owned()),
                Step::Reads(_) => None,
            })
            .collect()
    }

    #[test]
    fn it_answers_only_the_sign_in_status_and_a_session() {
        assert_eq!(
            call(&["auth", "status", "--json"], ""),
            (true, vec![SIGNED_IN.trim_end().to_owned()])
        );
        assert_eq!(call(&["auth", "status"], ""), (false, Vec::new()));
        assert_eq!(call(&["auth", "login"], ""), (false, Vec::new()));
        assert_eq!(call(&[], ""), (false, Vec::new()));
        assert_eq!(call(&["--print", "--verbose"], ""), (true, Vec::new()));
    }

    // The lines `recording` reads after its prompt, as its driver sends them.
    fn answers(recording: &str) -> String {
        steps(recording)
            .skip(1)
            .filter_map(|step| match step.unwrap() {
                Step::Reads(line) => Some(format!("{line}\n")),
                Step::Prints(_) => None,
            })
            .collect()
    }

    #[test]
    fn each_prompt_plays_the_turn_to_its_end_and_waits_for_the_answer_to_its_prompt() {
        let until_asked = printed(TURN).into_iter().take(3).collect::<Vec<_>>();

        let first = call(&["--print"], &prompt("Fix the tests"));
        let both = call(
            &["--print"],
            &(prompt("Fix the tests") + &answers(TURN) + &prompt("Again")),
        );

        assert_eq!(first, (true, until_asked.clone()));
        assert_eq!(both, (true, [printed(TURN), until_asked].concat()));
    }

    #[test]
    fn a_prompt_a_recording_starts_with_plays_that_recording() {
        let [_, deny, interrupt] = RECORDINGS;

        let denied = call(&["--print"], &(prompt("Tidy the readme") + &answers(deny)));
        let stopped = call(
            &["--print"],
            &(prompt("Rewrite the lexer") + &answers(interrupt)),
        );
        let not_json = call(&["--print"], "Rewrite the lexer\n");

        assert_eq!(denied, (true, printed(deny)));
        assert_eq!(stopped, (true, printed(interrupt)));
        assert_eq!(not_json.1, printed(TURN)[..3]);
    }

    #[test]
    fn a_recording_plays_for_a_prompt_that_mentions_the_agent_and_carries_quotes_and_each_prompt_is_kept()
     {
        let [_, deny, _] = RECORDINGS;
        let called = "@Claude Code Tidy the readme\\n\\n---\\nOthers wrote: Rewrite the lexer";
        let args = ["--print".to_owned()];
        let input = prompt(called) + &answers(deny) + &prompt("Again");
        let (mut output, mut heard) = (Vec::new(), Vec::new());

        answer(&args, input.as_bytes(), &mut output, &mut heard).unwrap();

        let printed_lines = String::from_utf8(output).unwrap();
        assert!(printed_lines.starts_with(&printed(deny).join("\n")));
        let heard = String::from_utf8(heard).unwrap();
        let kept: Vec<Value> = heard.lines().map(|line| json(line).unwrap()).collect();
        let first = "@Claude Code Tidy the readme\n\n---\nOthers wrote: Rewrite the lexer";
        assert_eq!(kept, [Value::from(first), Value::from("Again")]);
    }

    #[test]
    fn an_answer_the_recording_doesnt_have_is_an_error_that_shows_both() {
        let args = ["--print".to_owned()];
        let deny = "{\"type\":\"control_response\",\"response\":{}}";
        let input = prompt("Fix the tests") + deny + "\n";
        let mut output = Vec::new();

        let error = answer(&args, input.as_bytes(), &mut output, io::sink()).unwrap_err();

        assert!(error.to_string().ends_with(&format!("\nbut got {deny}")));
        assert!(
            error
                .to_string()
                .starts_with("expected {\"type\":\"control_response\"")
        );
        assert_eq!(String::from_utf8(output).unwrap().lines().count(), 3);
    }
}
