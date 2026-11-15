//! What the server sends the machines that run chats, over their connections.

use tokio::sync::mpsc;
use tokio::time::timeout;
use weacon_observability::Fault;
use weacon_protocol::{
    Capability, ChatId, ChatQuery, ChatQueryKind, Code, Envelope, MachineId, Message, WorkspaceId,
    code,
};

use crate::shared::Shared;

/// What a core must offer to be sent chats' commands and queries.
pub(crate) const SERVE: Code = code!("chat.serve");

/// Sends `messages` to `machine`, all of them or none, a command among them as its request's
/// `attempt`; refuses with `machine.offline` when it isn't connected, doesn't serve chats or
/// hasn't room for them all.
pub(crate) fn send_all(
    shared: &Shared,
    machine: &MachineId,
    messages: Vec<Message>,
    attempt: i64,
) -> Result<(), Fault> {
    let sender = shared.links.sender(machine, &Capability::new(SERVE));
    let sender = sender.and_then(|to| to.upgrade()).ok_or_else(offline)?;
    // Room first, so a command is remembered as sent only once it is.
    let permits = sender
        .try_reserve_many(messages.len())
        .map_err(|_| offline())?;
    for (permit, message) in permits.zip(messages) {
        permit.send(envelope(shared, machine, message, attempt));
    }
    Ok(())
}

/// One connection of a machine whose core serves chats, kept apart from any newer one.
pub(crate) struct Route {
    machine: MachineId,
    to: mpsc::WeakSender<Envelope>,
}

impl Route {
    /// `machine`'s connection open now, if its core serves chats.
    pub(crate) fn open_now(shared: &Shared, machine: &MachineId) -> Option<Self> {
        let to = shared.links.sender(machine, &Capability::new(SERVE))?;
        Some(Self {
            machine: machine.clone(),
            to,
        })
    }

    /// Sends `message` over this connection once its queue has room, waiting as long as the
    /// sockets' timing allows, a command as its request's `attempt`; refuses with
    /// `machine.offline` when it has ended or has no room.
    pub(crate) async fn deliver(
        &self,
        shared: &Shared,
        message: Message,
        attempt: i64,
    ) -> Result<(), Fault> {
        let sender = self.to.upgrade().ok_or_else(offline)?;
        let room = timeout(shared.sockets.room_within, sender.reserve()).await;
        let Ok(Ok(permit)) = room else {
            return Err(offline());
        };
        permit.send(envelope(shared, &self.machine, message, attempt));
        Ok(())
    }

    pub(crate) const fn machine(&self) -> &MachineId {
        &self.machine
    }

    /// Closes this connection, unless a newer one took over from it.
    pub(crate) fn close(&self, shared: &Shared) {
        if let Some(sender) = self.to.upgrade() {
            shared.links.close_link(&self.machine, &sender);
        }
    }
}

// `message` in an envelope of its own, remembered as `attempt` when it holds a command.
fn envelope(shared: &Shared, machine: &MachineId, message: Message, attempt: i64) -> Envelope {
    let id = shared.links.next_id();
    if let Message::ChatCommand(command) = &message {
        shared.chats.sent.sent(machine, id, command, attempt);
    }
    Envelope { id, message }
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
