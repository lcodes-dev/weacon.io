//! A chat's live updates from its machine, passed on to the sockets that follow it and kept by
//! no one.

use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatLive, Code, MachineId, RunEvent, code};

use crate::shared::Shared;

/// What a core offers, and the server offers, to pass a chat's live updates on.
pub(crate) const LIVE: Code = code!("chat.live");

/// Passes `live` from `machine` on to the chat's followers: text as it is written, and the tool
/// a permission prompt names, never its input. An update of a chat that isn't on `machine`, or
/// whose owner no longer owns it there, goes nowhere.
pub(crate) async fn pass(shared: &Shared, machine: &MachineId, mut live: ChatLive) {
    match &mut live.update.event {
        RunEvent::TextDelta { .. } => {}
        RunEvent::PermissionAsked { input, .. } => *input = serde_json::Value::Null,
        RunEvent::Started { .. }
        | RunEvent::Text { .. }
        | RunEvent::ToolCall { .. }
        | RunEvent::ToolDone { .. }
        | RunEvent::PermissionWithdrawn { .. }
        | RunEvent::PermissionAnswered { .. }
        | RunEvent::Finished { .. }
        | RunEvent::Newer => return,
    }
    match runs_on(shared, machine, &live).await {
        Ok(true) => shared.chats.live(live),
        Ok(false) => {}
        Err(fault) => {
            let fields = [
                ("machine_id", machine.as_str().into()),
                ("workspace_id", live.workspace.as_str().into()),
                ("chat_id", live.chat.as_str().into()),
                ("err", Field::error(&fault)),
            ];
            shared.log.warn("chat live update failed", &fields);
        }
    }
}

async fn runs_on(shared: &Shared, machine: &MachineId, live: &ChatLive) -> Result<bool, Fault> {
    let Some(place) = shared.db.find_workspace(&live.workspace).await? else {
        return Ok(false);
    };
    place.runs_on(&live.chat, machine).await
}
