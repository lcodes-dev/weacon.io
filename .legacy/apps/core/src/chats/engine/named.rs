//! What names a chat, its run and what went wrong, on a log line.

use weacon_observability::{Fault, Field};
use weacon_protocol::RunId;

use crate::runs::ChatKey;

// What names a chat's run on a log line.
pub(super) fn about(key: &ChatKey, run: &RunId) -> [(&'static str, Field); 3] {
    let [workspace, chat] = chat_about(key);
    [workspace, chat, ("run_id", run.as_str().into())]
}

// What names a chat on a log line.
pub(super) fn chat_about(key: &ChatKey) -> [(&'static str, Field); 2] {
    [
        ("workspace_id", key.workspace.as_str().into()),
        ("chat_id", key.chat.as_str().into()),
    ]
}

// What names a chat's run on a log line, and the fault it met.
pub(super) fn failed(key: &ChatKey, run: &RunId, fault: &Fault) -> [(&'static str, Field); 4] {
    let [workspace, chat, run] = about(key, run);
    [workspace, chat, run, ("err", Field::error(fault))]
}
