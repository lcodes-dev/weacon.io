//! A made-up agent session, streamed as the lines a core sends, and the fakes' command line.

use std::path::PathBuf;
use std::time::Duration;

use weacon_protocol::{Envelope, Message, RunEvent, RunId, RunUpdate};

/// A made-up session of one agent run, one `RunEvent` of JSON per line, with whole text blocks.
pub const SESSION: &str = include_str!("../sessions/transcript.jsonl");

// The run every replayed event belongs to.
const RUN: &str = "r-1";

// About four bytes of English per token.
const BYTES_PER_TOKEN: usize = 4;
// Each delta carries about this many bytes, as an agent streams a few tokens at a time.
const DELTA_BYTES: usize = 24;

/// The lines a core sends to stream `session` until at least `tokens` tokens of text have gone
/// out, as a recording for [`crate::replay()`]. Each text block is streamed as deltas, then sent
/// whole; the session repeats from the start as often as it takes.
///
/// # Errors
/// When a line of `session` isn't a `RunEvent`, or the session has no text.
pub fn recording(session: &str, tokens: usize) -> Result<String, String> {
    let events = session
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{error}: {line}")))
        .collect::<Result<Vec<RunEvent>, _>>()?;
    let per_pass: usize = events.iter().map(text_tokens).sum();
    if per_pass == 0 {
        return Err("the session has no text".to_owned());
    }
    let run = RunId::try_from(RUN.to_owned())?;
    let mut lines = Vec::new();
    for _ in 0..tokens.div_ceil(per_pass) {
        for event in &events {
            for event in deltas(event).into_iter().chain([event.clone()]) {
                lines.push(line(run.clone(), event)?);
            }
        }
    }
    Ok(lines.join("\n"))
}

/// A fake's command line after its name: `<dir> [tokens] [pace-ms]`, by default 100,000 tokens
/// at one message a millisecond; `None` without a folder or with a number that isn't one.
pub fn args(mut args: impl Iterator<Item = String>) -> Option<(PathBuf, usize, Duration)> {
    let dir = PathBuf::from(args.next()?);
    let tokens = args.next().map_or(Ok(100_000), |arg| arg.parse()).ok()?;
    let pace = args.next().map_or(Ok(1), |arg| arg.parse()).ok()?;
    Some((dir, tokens, Duration::from_millis(pace)))
}

fn text_tokens(event: &RunEvent) -> usize {
    text_of(event).map_or(0, |text| text.len().div_ceil(BYTES_PER_TOKEN))
}

fn text_of(event: &RunEvent) -> Option<&str> {
    if let RunEvent::Text { text } = event {
        Some(text)
    } else {
        None
    }
}

fn deltas(event: &RunEvent) -> Vec<RunEvent> {
    let Some(text) = text_of(event) else {
        return Vec::new();
    };
    let mut deltas = Vec::new();
    let mut piece = String::new();
    for character in text.chars() {
        piece.push(character);
        if piece.len() >= DELTA_BYTES {
            let text = std::mem::take(&mut piece);
            deltas.push(RunEvent::TextDelta { text });
        }
    }
    if !piece.is_empty() {
        deltas.push(RunEvent::TextDelta { text: piece });
    }
    deltas
}

fn line(run: RunId, event: RunEvent) -> Result<String, String> {
    let envelope = Envelope {
        id: 0,
        message: Message::Run(RunUpdate { run, event }),
    };
    envelope_text(&envelope).map(|json| format!("< {json}"))
}

/// `envelope` as the text of one frame.
pub(crate) fn envelope_text(envelope: &Envelope) -> Result<String, String> {
    let json = envelope.to_json().map_err(|error| error.to_string())?;
    String::from_utf8(json).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn events(recording: &str) -> Vec<Value> {
        recording
            .lines()
            .map(|line| {
                let envelope: Value =
                    serde_json::from_str(line.strip_prefix("< ").unwrap()).unwrap();
                assert_eq!(envelope["type"], "run");
                assert_eq!(envelope["run"], RUN);
                envelope["event"].clone()
            })
            .collect()
    }

    #[test]
    fn text_streams_as_deltas_then_whole_and_the_session_repeats_until_enough() {
        let text = "é".repeat(13);
        let session = format!(
            "{}\n\n{}\n",
            json!({ "event": "text", "text": text }),
            json!({ "event": "finished", "outcome": { "state": "completed" } })
        );
        let pass = [
            json!({ "event": "text_delta", "text": "é".repeat(12) }),
            json!({ "event": "text_delta", "text": "é" }),
            json!({ "event": "text", "text": text }),
            json!({ "event": "finished", "outcome": { "state": "completed" } }),
        ];

        assert_eq!(events(&recording(&session, 7).unwrap()), pass);
        assert_eq!(
            events(&recording(&session, 8).unwrap()),
            [pass.clone(), pass].concat()
        );
    }

    #[test]
    fn a_session_must_be_json_with_some_text() {
        let finished = json!({ "event": "finished", "outcome": { "state": "completed" } });

        assert_eq!(
            recording(&finished.to_string(), 1),
            Err("the session has no text".to_owned())
        );
        assert!(recording("{", 1).is_err_and(|error| error.ends_with(": {")));
    }

    #[test]
    fn the_recorded_session_has_text_and_tools() {
        let recorded = events(&recording(SESSION, 100_000).unwrap());
        let count = |kind: &str| {
            recorded
                .iter()
                .filter(|event| event["event"] == kind)
                .count()
        };

        assert!(count("text") * 589 >= 100_000);
        assert!(count("tool_call") > 30);
        assert_eq!(count("tool_call"), count("tool_done"));
    }

    #[test]
    fn the_command_line_needs_a_folder_and_has_defaults_for_the_rest() {
        let args = |list: &[&str]| args(list.iter().map(|arg| (*arg).to_owned()));

        assert_eq!(args(&[]), None);
        assert_eq!(
            args(&["dir"]),
            Some((PathBuf::from("dir"), 100_000, Duration::from_millis(1)))
        );
        assert_eq!(
            args(&["dir", "50", "0"]),
            Some((PathBuf::from("dir"), 50, Duration::ZERO))
        );
        assert_eq!(args(&["dir", "5O"]), None);
        assert_eq!(args(&["dir", "50", "slow"]), None);
    }
}
