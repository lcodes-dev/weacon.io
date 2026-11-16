//! A chat's live updates from its machine, passed on to the sockets that follow it and kept by
//! no one.

use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatLive, Code, MachineId, RunEvent, code};

use crate::db::Leg;
use crate::shared::Shared;

/// What a core offers, and the server offers, to pass a chat's live updates on.
pub(crate) const LIVE: Code = code!("chat.live");

/// Passes `live` from `machine` to its chat's followers, when the chat runs there. Text from
/// before the chat's last stored event is dropped; the rest is told the chat's own seq.
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
    match placed(shared, machine, &live).await {
        Ok(Some((seq, leg))) => {
            // Under the name the chat's stored events know the run by.
            let Some(run) = leg.chat_run(&live.update.run) else {
                return;
            };
            if let RunEvent::PermissionAsked { run: asked, .. } = &mut live.update.event {
                asked.clone_from(&run);
            }
            live.update.run = run;
            live.after = Some(seq);
            shared.chats.live(live);
        }
        Ok(None) => {}
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

// The chat's seq when `live` goes on, and the leg of it `machine` holds, whose member still
// owns it: the chat holds no event of that leg stored after the one `live` came after.
async fn placed(
    shared: &Shared,
    machine: &MachineId,
    live: &ChatLive,
) -> Result<Option<(u64, Leg)>, Fault> {
    let Some(place) = shared.db.find_workspace(&live.workspace).await? else {
        return Ok(None);
    };
    let Some(chat) = place.chat(&live.chat).await? else {
        return Ok(None);
    };
    let Some(leg) = place.leg_on(&live.chat, machine).await? else {
        return Ok(None);
    };
    let text = matches!(live.update.event, RunEvent::TextDelta { .. });
    let late = text && live.after.is_some_and(|after| after < leg.core_seq);
    Ok((!late).then_some((chat.seq, leg)))
}
