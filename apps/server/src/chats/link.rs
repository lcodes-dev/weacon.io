//! What the server sends the machines that run chats, over their connections.

use weacon_observability::Fault;
use weacon_protocol::{
    Capability, ChatId, ChatQuery, ChatQueryKind, Code, Envelope, MachineId, Message, WorkspaceId,
    code,
};

use crate::http::Shared;

/// What a core must offer to be sent chats' commands and queries.
pub(crate) const SERVE: Code = code!("chat.serve");

/// Sends `message` to `machine`, or refuses with `machine.offline` when it isn't connected,
/// doesn't serve chats or doesn't keep up.
pub(crate) fn send(shared: &Shared, machine: &MachineId, message: Message) -> Result<(), Fault> {
    let envelope = Envelope {
        id: shared.links.next_id(),
        message,
    };
    shared
        .links
        .send(machine, &Capability::from(SERVE), envelope)
        .map_err(|_| offline())
}

/// Asks `machine` for `chat`'s events after `after`, then each new one as it is committed.
pub(crate) fn follow(
    shared: &Shared,
    machine: &MachineId,
    workspace: &WorkspaceId,
    chat: &ChatId,
    after: u64,
) -> Result<(), Fault> {
    let query = ChatQuery {
        workspace: workspace.clone(),
        chat: chat.clone(),
        query: ChatQueryKind::Subscribe { after },
    };
    send(shared, machine, Message::ChatQuery(query))
}

pub(crate) fn offline() -> Fault {
    Fault::new(
        code!("machine.offline"),
        "The chat's machine isn't connected",
    )
}
