//! A stand-in for Claude Code in the end-to-end tests. It replays the recorded streams in
//! `crates/agents/replays/claude`, and answers the sign-in status question with made-up data.

use std::io::{self, BufRead, Write};

use serde_json::Value;

const TURN: &str = include_str!("../../agents/replays/claude/turn.replay");
const RECORDINGS: [&str; 3] = [
    TURN,
    include_str!("../../agents/replays/claude/deny.replay"),
    include_str!("../../agents/replays/claude/interrupt.replay"),
];

/// What `claude auth status --json` prints for a claude.ai sign-in.
pub const SIGNED_IN: &str = "{\"authMethod\":\"claude.ai\"}\n";

/// Answers one call with `args`: the sign-in status, or a stream-json session on `input` and
/// `output`. False for any other call, which it doesn't answer.
///
/// # Errors
/// When `input` or `output` fails.
pub fn answer(args: &[String], input: impl BufRead, mut output: impl Write) -> io::Result<bool> {
    match args.first().map(String::as_str) {
        Some("auth") if args == ["auth", "status", "--json"] => {
            output.write_all(SIGNED_IN.as_bytes())?;
            Ok(true)
        }
        Some("--print") => converse(input, output).map(|()| true),
        _ => Ok(false),
    }
}

// For each prompt, plays the recording that starts with it, else `TURN`: prints its `< ` lines
// and reads one line for each `> ` line after the prompt. Ends with `input`.
fn converse(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut lines = input.lines();
    while let Some(prompt) = lines.next().transpose()? {
        for step in recording_for(&prompt).lines().skip(1) {
            if let Some(line) = step.strip_prefix("< ") {
                writeln!(output, "{line}")?;
                output.flush()?;
            } else if step.starts_with("> ") && lines.next().transpose()?.is_none() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn recording_for(prompt: &str) -> &'static str {
    let asked = content(prompt);
    RECORDINGS
        .into_iter()
        .find(|recording| {
            let first = recording
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("> "));
            first.and_then(content) == asked
        })
        .unwrap_or(TURN)
}

fn content(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .pointer("/message/content")
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Whether it answered, and the lines it printed.
    fn call(args: &[&str], input: &str) -> (bool, Vec<String>) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut output = Vec::new();
        let answered = answer(&args, input.as_bytes(), &mut output).unwrap();
        let printed = String::from_utf8(output).unwrap();
        (answered, printed.lines().map(str::to_owned).collect())
    }

    fn prompt(text: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n")
    }

    // The `< ` lines of `recording`.
    fn printed(recording: &str) -> Vec<String> {
        recording
            .lines()
            .filter_map(|line| line.strip_prefix("< "))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn it_answers_only_the_sign_in_status_and_a_session() {
        let mut output = Vec::new();
        let status = ["auth", "status", "--json"].map(str::to_owned);

        assert!(answer(&status, "".as_bytes(), &mut output).unwrap());
        assert_eq!(output, SIGNED_IN.as_bytes());
        assert_eq!(call(&["auth", "status"], ""), (false, Vec::new()));
        assert_eq!(call(&["auth", "login"], ""), (false, Vec::new()));
        assert_eq!(call(&[], ""), (false, Vec::new()));
        assert_eq!(call(&["--print", "--verbose"], ""), (true, Vec::new()));
    }

    #[test]
    fn each_prompt_plays_the_turn_to_its_end_and_waits_for_the_answer_to_its_prompt() {
        let allow = "{\"type\":\"control_response\"}\n";
        let until_asked = printed(TURN).into_iter().take(3).collect::<Vec<_>>();

        let first = call(&["--print"], &prompt("Fix the tests"));
        let both = call(
            &["--print"],
            &(prompt("Fix the tests") + allow + &prompt("Again")),
        );

        assert_eq!(first, (true, until_asked.clone()));
        assert_eq!(both, (true, [printed(TURN), until_asked].concat()));
    }

    #[test]
    fn a_prompt_a_recording_starts_with_plays_that_recording() {
        let [_, deny, interrupt] = RECORDINGS;

        let denied = call(&["--print"], &(prompt("Tidy the readme") + "{}\n{}\n"));
        let stopped = call(&["--print"], &(prompt("Rewrite the lexer") + "{}\n"));
        let not_json = call(&["--print"], "Rewrite the lexer\n");

        assert_eq!(denied, (true, printed(deny)));
        assert_eq!(stopped, (true, printed(interrupt)));
        assert_eq!(not_json.1, printed(TURN)[..3]);
    }
}
