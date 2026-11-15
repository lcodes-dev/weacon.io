//! What a core answers under a chat command's envelope: a refusal, which ends the command.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use weacon_observability::Field;
use weacon_protocol::{
    ChatCommand, ChatEvent, ChatId, ErrorBody, MachineId, Message, Outcome, Receipt, RequestId,
    WorkspaceId,
};

use crate::shared::Shared;
use crate::socket::Closed;

/// How many chat commands sent are remembered for their refusals, which come at once.
const KEPT: usize = 4096;

/// The chat commands sent to machines lately, by machine and envelope id.
#[derive(Debug, Default)]
pub(crate) struct Sent {
    kept: Mutex<Kept>,
}

#[derive(Debug, Default)]
struct Kept {
    order: VecDeque<(MachineId, u32)>,
    asked: HashMap<(MachineId, u32), Asked>,
}

/// The command an envelope carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    workspace: WorkspaceId,
    chat: ChatId,
    request: RequestId,
}

impl Sent {
    /// Remembers that the envelope `id` carried `command` to `machine`.
    pub(crate) fn sent(&self, machine: &MachineId, id: u32, command: &ChatCommand) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (machine.clone(), id);
        let asked = Asked {
            workspace: command.workspace.clone(),
            chat: command.chat.clone(),
            request: command.request.clone(),
        };
        if kept.asked.insert(key.clone(), asked).is_none() {
            kept.order.push_back(key);
        }
        while kept.order.len() > KEPT {
            if let Some(oldest) = kept.order.pop_front() {
                kept.asked.remove(&oldest);
            }
        }
    }

    /// The command the envelope `id` carried to `machine`, if it was one, now answered.
    pub(crate) fn answered(&self, machine: &MachineId, id: u32) -> Option<Asked> {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (machine.clone(), id);
        let asked = kept.asked.remove(&key)?;
        kept.order.retain(|each| *each != key);
        Some(asked)
    }
}

/// What a core's `answer` to the chat command `asked` makes of it. A refusal for now closes
/// the link, so the command is sent again once the core connects again; any other is the
/// command's final receipt, stored in its chat for its feeds to hear.
pub(crate) async fn take(
    shared: &Shared,
    machine: &MachineId,
    asked: Asked,
    answer: Message,
) -> Result<(), Closed> {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "a core answers a command only with an error or a receipt"
    )]
    let outcome = match answer {
        Message::Error(ErrorBody {
            retryable: true, ..
        }) => return Err(Closed),
        Message::Error(ErrorBody { code, .. }) => Outcome::Failed { code },
        Message::Receipt(Receipt { outcome, .. }) => outcome,
        _ => return Ok(()),
    };
    if matches!(outcome, Outcome::Accepted | Outcome::Newer) {
        return Ok(());
    }
    let receipt = Receipt {
        request: asked.request.clone(),
        outcome,
    };
    let fields = [
        ("machine_id", machine.as_str().into()),
        ("workspace_id", asked.workspace.as_str().into()),
        ("chat_id", asked.chat.as_str().into()),
    ];
    match end(shared, &asked, receipt).await {
        Ok(()) => {
            shared.wake_feeds(&asked.workspace);
            shared.chats.stored(asked.workspace, asked.chat);
        }
        Err(fault) => {
            let fields = [&fields[..], &[("err", Field::error(&fault))]].concat();
            shared.log.warn("chat refusal not kept", &fields);
        }
    }
    Ok(())
}

async fn end(
    shared: &Shared,
    asked: &Asked,
    receipt: Receipt,
) -> Result<(), weacon_observability::Fault> {
    let Some(place) = shared.db.find_workspace(&asked.workspace).await? else {
        return Ok(());
    };
    let event = ChatEvent::Receipt { receipt };
    place.add_receipt(&asked.chat, &event, shared.now()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{command, id};

    #[test]
    fn a_command_is_answered_once_on_its_own_machine_and_the_oldest_are_let_go() {
        let sent = Sent::default();
        let (ours, theirs) = (id::<MachineId>("mc-1"), id::<MachineId>("mc-2"));
        let asked = command("r-1", "c-1", "Go");
        sent.sent(&ours, 1, &asked);

        assert_eq!(sent.answered(&theirs, 1), None);
        let answered = sent.answered(&ours, 1).unwrap();
        assert_eq!(answered.request.as_str(), "r-1");
        assert_eq!(answered.chat.as_str(), "c-1");
        assert_eq!(sent.answered(&ours, 1), None);
        for envelope in 0..=u32::try_from(KEPT).unwrap() {
            sent.sent(&ours, envelope, &asked);
        }
        assert_eq!(sent.answered(&ours, 0), None);
        assert!(sent.answered(&ours, 1).is_some());
        assert!(sent.answered(&ours, u32::try_from(KEPT).unwrap()).is_some());
    }
}
