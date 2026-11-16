//! What the server sends the machines that run chats, over their connections.

use weacon_observability::Fault;
use weacon_protocol::{
    ChatId, ChatQuery, ChatQueryKind, Envelope, MachineId, Message, WorkspaceId, code,
};

use crate::shared::Shared;

/// Sends `messages` to `machine` under their envelope ids, all of them or none, refusing with
/// `machine.offline` when it isn't connected or hasn't room for them all.
pub(crate) fn send_all(
    shared: &Shared,
    machine: &MachineId,
    messages: Vec<(u32, Message)>,
) -> Result<(), Fault> {
    let envelopes = messages
        .into_iter()
        .map(|(id, message)| Envelope { id, message });
    shared
        .links
        .send_all(machine, envelopes.collect())
        .map_err(|_| offline())
}

/// Asks for `chat`'s events after `after`, then each new one as it is committed.
pub(crate) fn follow(workspace: &WorkspaceId, chat: &ChatId, after: u64) -> Message {
    Message::ChatQuery(ChatQuery {
        workspace: workspace.clone(),
        chat: chat.clone(),
        query: ChatQueryKind::Subscribe { after },
    })
}

pub(super) fn offline() -> Fault {
    Fault::new(
        code!("machine.offline"),
        "The chat's machine isn't connected",
    )
}
