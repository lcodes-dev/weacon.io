//! What the server sends the machines that run chats, over their connections.

use weacon_observability::Fault;
use weacon_protocol::{
    Capability, ChatId, ChatQuery, ChatQueryKind, Code, Envelope, MachineId, Message, WorkspaceId,
    code,
};

use crate::shared::Shared;

/// What a core must offer to be sent chats' commands and queries.
pub(crate) const SERVE: Code = code!("chat.serve");

/// Sends `messages` to `machine`, all of them or none, refusing with `machine.offline` when it
/// isn't connected, doesn't serve chats or hasn't room for them all.
pub(crate) fn send_all(
    shared: &Shared,
    machine: &MachineId,
    messages: Vec<Message>,
) -> Result<(), Fault> {
    let envelopes = messages.into_iter().map(|message| Envelope {
        id: shared.links.next_id(),
        message,
    });
    let serve = Capability::new(SERVE);
    shared
        .links
        .send_all(machine, &serve, envelopes.collect())
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
