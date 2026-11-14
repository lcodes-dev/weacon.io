//! A member's command to a chat: checked here, then relayed to the chat's machine.

use weacon_observability::Fault;
use weacon_protocol::{ChatCommand, ChatState, MachineId, Message, code};

use super::admit::{Admitted, admit, detached, not_owner};
use crate::auth::Member;
use crate::db::chat_not_found;
use crate::http::Shared;

/// Takes `command` from `member`, refusing what the chat's owner or machine doesn't allow.
pub(crate) async fn take(
    member: &Member,
    shared: &Shared,
    command: ChatCommand,
) -> Result<Option<Message>, Fault> {
    let workspace = &member.workspace;
    // Another workspace's chat answers as one there isn't.
    if command.workspace != *workspace.id() {
        return Err(chat_not_found());
    }
    let id = command.chat.clone();
    let chat = workspace.chat(&id).await?;
    let asker = workspace.asker(&member.id).await?.ok_or_else(not_owner)?;
    let account = asker.account.clone();
    let admitted = admit(chat.as_ref(), asker, command)?;
    let machine = match (&admitted, chat) {
        (Admitted::New { machine, .. }, _) => machine.clone(),
        (Admitted::Existing(_), Some(chat)) => chat.machine,
        (Admitted::Existing(_), None) => return Err(chat_not_found()),
    };
    let owned = shared.db.machines_of(&account).await?;
    if !owned.iter().any(|summary| summary.id == machine) {
        if matches!(admitted, Admitted::New { .. }) {
            return Err(Fault::new(
                code!("machine.not_found"),
                "There is no such machine",
            ));
        }
        // A chat whose machine was unpaired can still be read, never run.
        workspace.set_chat_state(&id, ChatState::Detached).await?;
        return Err(detached());
    }
    relay(&machine).map(|()| None)
}

// Until machines' connections can be reached, every machine is offline.
fn relay(_machine: &MachineId) -> Result<(), Fault> {
    Err(Fault::new(
        code!("machine.offline"),
        "The chat's machine isn't connected",
    ))
}
