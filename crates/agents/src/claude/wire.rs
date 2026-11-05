//! The lines `claude` exchanges in its stream-json mode, as far as the driver needs them.

use serde::Deserialize;
use serde_json::{Value, json};
use weacon_protocol::AgentSession;

/// Sent back when a person denies a tool; the agent reads it.
const DENIED: &str = "The person declined this tool use.";

/// Sent back for a request the driver doesn't handle.
const UNSUPPORTED: &str = "Weacon doesn't handle this request.";

/// One line `claude` prints. Lines that don't parse are skipped, as a newer `claude` may add some.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Line {
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
pub(crate) enum System {
    Init {
        session_id: AgentSession,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Message {
    content: Value,
}

impl Message {
    /// The blocks this build knows; a plain string content has none of interest.
    pub(crate) fn blocks(self) -> Vec<Block> {
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
pub(crate) enum Block {
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
pub(crate) enum Request {
    CanUseTool {
        tool_name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

pub(crate) fn user(text: &str) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": text },
        "parent_tool_use_id": null,
        "session_id": "",
    })
}

pub(crate) fn permission(request_id: &str, allow: bool, input: &Value) -> Value {
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": DENIED })
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
}

pub(crate) fn unsupported(request_id: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": UNSUPPORTED },
    })
}

pub(crate) fn interrupt(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_texts_added_to_the_agents_context_keep_their_size() {
        assert_eq!([DENIED.len(), UNSUPPORTED.len()], [34, 35]);
    }
}
