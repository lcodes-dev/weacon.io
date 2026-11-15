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

/// Sends `messages` to `machine`, all of them or none, refusing with `machine.offline` when it
/// isn't connected, doesn't serve chats or hasn't room for them all.
pub(crate) fn send_all(
    shared: &Shared,
    machine: &MachineId,
    messages: Vec<Message>,
) -> Result<(), Fault> {
    let envelopes = messages
        .into_iter()
        .map(|message| envelope(shared, machine, message));
    let serve = Capability::new(SERVE);
    shared
        .links
        .send_all(machine, &serve, envelopes.collect())
        .map_err(|_| offline())
}

/// One connection of a machine whose core serves chats, kept apart from any newer one.
pub(crate) struct Route {
    machine: MachineId,
    to: mpsc::WeakSender<Envelope>,
}

impl Route {
    /// `machine`'s connection open now, if its core serves chats.
    pub(crate) fn open_now(shared: &Shared, machine: &MachineId) -> Option<Self> {
        let sender = shared.links.sender(machine, &Capability::new(SERVE))?;
        Some(Self {
            machine: machine.clone(),
            to: sender.downgrade(),
        })
    }

    /// Sends `message` over this connection once its queue has room, waiting as long as the
    /// sockets' timing allows; refuses with `machine.offline` when it has ended or has no room.
    pub(crate) async fn deliver(&self, shared: &Shared, message: Message) -> Result<(), Fault> {
        let sender = self.to.upgrade().ok_or_else(offline)?;
        let envelope = envelope(shared, &self.machine, message);
        match timeout(shared.sockets.room_within, sender.send(envelope)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(offline()),
        }
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

// `message` in an envelope of its own, remembered when it holds a command.
fn envelope(shared: &Shared, machine: &MachineId, message: Message) -> Envelope {
    let id = shared.links.next_id();
    if let Message::ChatCommand(command) = &message {
        shared.chats.sent.sent(machine, id, command);
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
