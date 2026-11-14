//! What is kept of a run's event too large for one frame.

use serde_json::Value;
use weacon_protocol::RunEvent;

// What is kept of a text too long for one frame: it still fits when every byte is escaped.
const TEXT_KEPT: usize = 128 << 10;

// `event` without what made it too large for one frame.
pub(super) fn cut(event: &RunEvent) -> RunEvent {
    match event.clone() {
        RunEvent::Text { text } => {
            let kept = text
                .char_indices()
                .take_while(|(at, _)| *at < TEXT_KEPT)
                .map(|(_, letter)| letter);
            RunEvent::Text {
                text: kept.collect(),
            }
        }
        RunEvent::ToolCall { call, tool, .. } => RunEvent::ToolCall {
            call,
            tool,
            input: Value::Null,
        },
        RunEvent::PermissionAsked {
            run, prompt, tool, ..
        } => RunEvent::PermissionAsked {
            run,
            prompt,
            tool,
            input: Value::Null,
        },
        whole @ (RunEvent::Started { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ToolDone { .. }
        | RunEvent::PermissionWithdrawn { .. }
        | RunEvent::Finished { .. }
        | RunEvent::Newer) => whole,
    }
}
