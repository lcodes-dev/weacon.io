//! The lines of the session files Claude Code keeps under its config folder's `projects`.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use super::wire::{Block, Message, event};
use crate::transcript::{Said, Transcript};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line {
    User {
        message: Message,
        #[serde(default, rename = "isSidechain")]
        sidechain: bool,
        // Text Claude Code adds for the model or the screen, not what the person typed.
        #[serde(default, rename = "isMeta")]
        meta: bool,
        #[serde(default, rename = "isCompactSummary")]
        summary: bool,
        #[serde(default, rename = "isVisibleInTranscriptOnly")]
        screen_only: bool,
    },
    Assistant {
        message: Message,
        #[serde(default, rename = "isSidechain")]
        sidechain: bool,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Place {
    cwd: Option<PathBuf>,
}

/// Adds `line` to `transcript`; false when it doesn't parse.
pub(crate) fn line(line: &[u8], transcript: &mut Transcript) -> bool {
    let Ok(parsed) = serde_json::from_slice::<Line>(line) else {
        return false;
    };
    if transcript.cwd.is_none() {
        transcript.cwd = serde_json::from_slice::<Place>(line)
            .ok()
            .and_then(|place| place.cwd);
    }
    match parsed {
        Line::User {
            message,
            sidechain: false,
            meta: false,
            summary: false,
            screen_only: false,
        } => from_person(message, transcript),
        Line::Assistant {
            message,
            sidechain: false,
        } => {
            let events = message.blocks().into_iter().filter_map(event);
            transcript.said.extend(events.map(Said::Event));
        }
        Line::User { .. } | Line::Assistant { .. } | Line::Other => {}
    }
    true
}

fn from_person(message: Message, transcript: &mut Transcript) {
    if let Value::String(text) = &message.content {
        transcript.said.push(Said::Prompt(text.clone()));
        return;
    }
    for block in message.blocks() {
        if let Block::Text { text } = block {
            transcript.said.push(Said::Prompt(text));
        } else {
            transcript.said.extend(event(block).map(Said::Event));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{AgentKind, RunEvent};

    use super::*;
    use crate::test_support::{jsonl, text, tool_done};

    fn read(lines: &[Value]) -> Transcript {
        Transcript::read(AgentKind::Claude, jsonl(lines).as_bytes()).unwrap()
    }

    #[test]
    fn a_session_reads_as_prompts_texts_and_tool_calls_in_order() {
        let transcript = read(&[
            json!({ "type": "queue-operation", "operation": "enqueue" }),
            json!({ "type": "user", "cwd": "/work/app", "message": { "role": "user", "content": "Fix the build" } }),
            json!({ "type": "assistant", "cwd": "/elsewhere", "message": { "content": [
                { "type": "thinking", "thinking": "hidden" },
                { "type": "text", "text": "Looking" },
                { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "make" } },
            ] } }),
            json!({ "type": "user", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "failed" },
            ] } }),
            json!({ "type": "user", "message": { "content": [
                { "type": "text", "text": "Try again" },
                { "type": "image", "source": {} },
            ] } }),
        ]);

        assert_eq!(
            transcript,
            Transcript {
                cwd: Some(PathBuf::from("/work/app")),
                said: vec![
                    Said::Prompt("Fix the build".to_owned()),
                    Said::Event(text("Looking")),
                    Said::Event(RunEvent::ToolCall {
                        call: "t1".to_owned(),
                        tool: "Bash".to_owned(),
                        input: json!({ "command": "make" }),
                    }),
                    Said::Event(tool_done("t1", true)),
                    Said::Prompt("Try again".to_owned()),
                ],
                skipped: 0,
            }
        );
    }

    #[test]
    fn subagents_and_text_added_for_the_model_are_left_out() {
        let transcript = read(&[
            json!({ "type": "user", "isMeta": true, "message": { "content": "Caveat" } }),
            json!({ "type": "user", "isCompactSummary": true, "message": { "content": "Summary" } }),
            json!({ "type": "user", "isVisibleInTranscriptOnly": true, "message": { "content": "Shown" } }),
            json!({ "type": "user", "isSidechain": true, "message": { "content": "Explore" } }),
            json!({ "type": "assistant", "isSidechain": true, "message": { "content": [
                { "type": "text", "text": "Found it" },
            ] } }),
        ]);

        assert_eq!(transcript, Transcript::default());
    }

    #[test]
    fn a_known_line_of_the_wrong_shape_is_counted() {
        let transcript = read(&[
            json!({ "type": "user" }),
            json!({ "type": "assistant", "isSidechain": "yes", "message": { "content": [] } }),
            json!({ "type": "user", "cwd": 7, "message": { "content": "Hi" } }),
        ]);

        assert_eq!(transcript.skipped, 2);
        assert_eq!(transcript.cwd, None);
        assert_eq!(transcript.said, vec![Said::Prompt("Hi".to_owned())]);
    }
}
