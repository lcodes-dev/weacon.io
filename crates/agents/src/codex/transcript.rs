//! The lines of the session files Codex keeps under its home's `sessions`.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use weacon_protocol::RunEvent;

use crate::transcript::{Step, Transcript};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line {
    SessionMeta {
        payload: Meta,
    },
    EventMsg {
        payload: Event,
    },
    ResponseItem {
        payload: Item,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Meta {
    cwd: PathBuf,
}

// The person's prompts and the agent's texts, without the context Codex adds for the model.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    UserMessage {
        message: String,
    },
    AgentMessage {
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Item {
    FunctionCall {
        call_id: String,
        name: String,
        // JSON, written as a string.
        arguments: String,
    },
    CustomToolCall {
        call_id: String,
        name: String,
        input: String,
    },
    FunctionCallOutput {
        call_id: Option<String>,
    },
    CustomToolCallOutput {
        call_id: String,
    },
    #[serde(other)]
    Other,
}

pub(crate) fn read_line(line: &[u8], transcript: &mut Transcript) -> serde_json::Result<()> {
    let parsed = serde_json::from_slice::<Line>(line)?;
    match parsed {
        Line::SessionMeta { payload } => {
            transcript.cwd.get_or_insert(payload.cwd);
        }
        Line::EventMsg {
            payload: Event::UserMessage { message },
        } => transcript.steps.push(Step::Prompt(message)),
        Line::EventMsg {
            payload: Event::AgentMessage { message },
        } => transcript
            .steps
            .push(Step::Event(RunEvent::Text { text: message })),
        Line::ResponseItem { payload } => transcript.steps.extend(event(payload).map(Step::Event)),
        Line::EventMsg {
            payload: Event::Other,
        }
        | Line::Other => {}
    }
    Ok(())
}

fn event(item: Item) -> Option<RunEvent> {
    let (call, tool, input) = match item {
        Item::FunctionCall {
            call_id,
            name,
            arguments,
        } => {
            let input = serde_json::from_str(&arguments).unwrap_or(Value::String(arguments));
            (call_id, name, input)
        }
        Item::CustomToolCall {
            call_id,
            name,
            input,
        } => (call_id, name, Value::String(input)),
        Item::FunctionCallOutput {
            call_id: Some(call),
        }
        | Item::CustomToolCallOutput { call_id: call } => {
            // The files don't say whether a tool failed.
            return Some(RunEvent::ToolDone {
                call,
                failed: false,
            });
        }
        Item::FunctionCallOutput { call_id: None } | Item::Other => return None,
    };
    Some(RunEvent::ToolCall { call, tool, input })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::AgentKind;

    use super::*;
    use crate::test_support::{jsonl, text, tool_call, tool_done};

    fn read(lines: &[Value]) -> Transcript {
        Transcript::read(AgentKind::Codex, jsonl(lines).as_bytes()).unwrap()
    }

    fn item(payload: &Value) -> Value {
        json!({ "timestamp": "2026-11-13T10:00:00.000Z", "type": "response_item", "payload": payload })
    }

    fn event_msg(payload: &Value) -> Value {
        json!({ "timestamp": "2026-11-13T10:00:00.000Z", "type": "event_msg", "payload": payload })
    }

    #[test]
    fn a_session_reads_as_prompts_texts_and_tool_calls_in_order() {
        let transcript = read(&[
            json!({ "type": "session_meta", "payload": { "id": "x", "cwd": "/work/app" } }),
            json!({ "type": "turn_context", "payload": { "cwd": "/elsewhere" } }),
            item(&json!({ "type": "message", "role": "user", "content": [
                { "type": "input_text", "text": "<environment_context>" },
            ] })),
            event_msg(&json!({ "type": "user_message", "message": "Fix the build" })),
            item(&json!({ "type": "reasoning", "summary": [] })),
            item(
                &json!({ "type": "function_call", "name": "shell", "call_id": "c1",
                "arguments": "{\"command\":[\"make\"]}" }),
            ),
            item(&json!({ "type": "function_call_output", "call_id": "c1", "output": "ok" })),
            item(
                &json!({ "type": "custom_tool_call", "name": "apply_patch", "call_id": "c2",
                "input": "*** Begin Patch" }),
            ),
            item(&json!({ "type": "custom_tool_call_output", "call_id": "c2", "output": "done" })),
            event_msg(&json!({ "type": "agent_message", "message": "Fixed" })),
            event_msg(&json!({ "type": "token_count", "info": null })),
        ]);

        assert_eq!(
            transcript,
            Transcript {
                cwd: Some(PathBuf::from("/work/app")),
                steps: vec![
                    Step::Prompt("Fix the build".to_owned()),
                    Step::Event(tool_call("c1", "shell", json!({ "command": ["make"] }))),
                    Step::Event(tool_done("c1", false)),
                    Step::Event(tool_call("c2", "apply_patch", json!("*** Begin Patch"))),
                    Step::Event(tool_done("c2", false)),
                    Step::Event(text("Fixed")),
                ],
                skipped: 0,
                end: 1343,
            }
        );
    }

    #[test]
    fn arguments_that_arent_json_are_kept_as_text_and_the_first_folder_wins() {
        let transcript = read(&[
            json!({ "type": "session_meta", "payload": { "cwd": "/first" } }),
            json!({ "type": "session_meta", "payload": { "cwd": "/second" } }),
            item(
                &json!({ "type": "function_call", "name": "shell", "call_id": "c1",
                "arguments": "make" }),
            ),
            item(&json!({ "type": "function_call_output", "output": "ok" })),
        ]);

        assert_eq!(transcript.cwd, Some(PathBuf::from("/first")));
        assert_eq!(
            transcript.steps,
            vec![Step::Event(tool_call("c1", "shell", json!("make")))]
        );
    }

    #[test]
    fn a_known_line_of_the_wrong_shape_is_counted() {
        let transcript = read(&[
            json!({ "type": "session_meta", "payload": {} }),
            event_msg(&json!({ "type": "user_message" })),
            item(&json!({ "type": "function_call", "name": "shell" })),
            json!({ "type": "compacted", "payload": { "message": "" } }),
        ]);

        assert_eq!(
            transcript,
            Transcript {
                skipped: 3,
                end: 291,
                ..Transcript::default()
            }
        );
    }
}
