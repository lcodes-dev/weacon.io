//! The lines `claude` exchanges in its stream-json mode, as far as the driver needs them.

use serde::Deserialize;
use serde_json::{Value, json};
use weacon_protocol::{AgentSession, RunEvent};

use crate::wire::{Asked, Effect, UNSUPPORTED};

/// Sent back when a person denies a tool; the agent reads it.
const DENIED: &str = "The person declined this tool use.";

/// One line `claude` prints. Lines that don't parse are skipped, as a newer `claude` may add some.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line {
    System(System),
    Assistant {
        message: Message,
    },
    User {
        message: Message,
    },
    Result {
        subtype: String,
        #[serde(default)]
        is_error: bool,
    },
    ControlRequest {
        request_id: String,
        request: Request,
    },
    ControlCancelRequest {
        request_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
enum System {
    Init {
        session_id: AgentSession,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: Value,
}

impl Message {
    /// The blocks this build knows; a plain string content has none of interest.
    fn blocks(self) -> Vec<Block> {
        let Value::Array(blocks) = self.content else {
            return Vec::new();
        };
        blocks
            .into_iter()
            .filter_map(|block| serde_json::from_value(block).ok())
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
enum Request {
    CanUseTool {
        tool_name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

/// Claude's side of a run: the interrupts it has asked for, to number them.
#[derive(Default)]
pub(crate) struct Wire {
    interrupts: u32,
}

impl Wire {
    pub(crate) fn interrupt(&mut self) -> Vec<Effect> {
        self.interrupts += 1;
        vec![Effect::Send(json!({
            "type": "control_request",
            "request_id": format!("weacon-{}", self.interrupts),
            "request": { "subtype": "interrupt" },
        }))]
    }
}

pub(crate) fn prompt(text: &str) -> Vec<Effect> {
    vec![Effect::Send(json!({
        "type": "user",
        "message": { "role": "user", "content": text },
        "parent_tool_use_id": null,
        "session_id": "",
    }))]
}

pub(crate) fn answer(request: &Value, allow: bool, input: &Value) -> Vec<Effect> {
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": DENIED })
    };
    vec![Effect::Send(json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request, "response": response },
    }))]
}

pub(crate) fn read(line: &[u8]) -> Vec<Effect> {
    let Ok(line) = serde_json::from_slice(line) else {
        return Vec::new();
    };
    match line {
        Line::System(System::Init { session_id }) => vec![Effect::Started(session_id)],
        Line::Assistant { message } | Line::User { message } => {
            message.blocks().into_iter().filter_map(said).collect()
        }
        Line::Result { subtype, is_error } => vec![Effect::Finished {
            ok: subtype == "success" && !is_error,
        }],
        Line::ControlRequest {
            request_id,
            request: Request::CanUseTool { tool_name, input },
        } => vec![Effect::Asked(Asked {
            request: Value::String(request_id),
            tool: tool_name,
            input,
        })],
        Line::ControlRequest { request_id, .. } => vec![Effect::Send(json!({
            "type": "control_response",
            "response": { "subtype": "error", "request_id": request_id, "error": UNSUPPORTED },
        }))],
        Line::ControlCancelRequest { request_id } => vec![Effect::Withdrawn {
            request: Value::String(request_id),
        }],
        Line::System(System::Other) | Line::Other => Vec::new(),
    }
}

fn said(block: Block) -> Option<Effect> {
    let event = match block {
        Block::Text { text } => RunEvent::Text { text },
        Block::ToolUse { id, name, input } => RunEvent::ToolCall {
            call: id,
            tool: name,
            input,
        },
        Block::ToolResult {
            tool_use_id,
            is_error,
        } => RunEvent::ToolDone {
            call: tool_use_id,
            failed: is_error,
        },
        Block::Other => return None,
    };
    Some(Effect::Event(event))
}
