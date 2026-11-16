//! What the server sends the machines that run chats, over their connections.

use weacon_observability::Fault;
use weacon_protocol::{
    Capability, ChatId, ChatQuery, ChatQueryKind, Code, Envelope, MachineId, Message, WorkspaceId,
    code,
};

use crate::shared::Shared;

/// What a core must offer to be sent chats' queries.
pub(crate) const SERVE: Code = code!("chat.serve");
/// What a core must offer to be sent chats' commands: it answers each, taken or refused.
pub(crate) const ANSWERS: Code = code!("chat.answers");

/// Sends `messages` to `machine` under their envelope ids, all of them or none, refusing with
/// `message.unsupported` when its core serves chats but doesn't offer `needs`, and with
/// `machine.offline` when it isn't connected or hasn't room for them all.
pub(crate) fn send_all(
    shared: &Shared,
    machine: &MachineId,
    needs: Code,
    messages: Vec<(u32, Message)>,
) -> Result<(), Fault> {
    let envelopes = messages
        .into_iter()
        .map(|(id, message)| Envelope { id, message });
    let needs = Capability::new(needs);
    let sent = shared.links.send_all(machine, &needs, envelopes.collect());
    // A core that serves chats but can't do what they need is there, only too old.
    let there = |offer: &Capability| shared.links.offers(machine, offer);
    sent.map_err(|_| {
        if there(&Capability::new(SERVE)) && !there(&needs) {
            super::unsupported()
        } else {
            offline()
        }
    })
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
