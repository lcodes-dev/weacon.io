//! What a core answers under a chat command's envelope: a refusal, which ends the command.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatCommand, ChatId, ErrorBody, MachineId, Message, Outcome, Receipt, RequestId, WorkspaceId,
};

use crate::db::Workspace;
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
#[derive(Debug, PartialEq, Eq)]
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
        // One is added at a time, so letting go of the oldest keeps at most `KEPT`.
        if kept.order.len() > KEPT
            && let Some(oldest) = kept.order.pop_front()
        {
            kept.asked.remove(&oldest);
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

/// What a core's `answer` to the chat command `asked` makes of it. A refusal for now marks
/// the command refused and closes the link, so it is sent again once the core connects again;
/// any other is the command's final receipt, stored in its chat for its feeds to hear.
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
        }) => {
            if let Err(fault) = refused(shared, &asked).await {
                warn(shared, machine, &asked, "chat refusal not marked", &fault);
            }
            return Err(Closed);
        }
        Message::Error(ErrorBody { code, .. }) => Outcome::Failed { code },
        Message::Receipt(Receipt { outcome, .. }) => outcome,
        _ => return Ok(()),
    };
    if matches!(outcome, Outcome::Accepted | Outcome::Newer) {
        return Ok(());
    }
    if let Err(fault) = failed(shared, &asked, outcome).await {
        warn(shared, machine, &asked, "chat refusal not kept", &fault);
    }
    Ok(())
}

async fn failed(shared: &Shared, asked: &Asked, outcome: Outcome) -> Result<(), Fault> {
    let Some(place) = shared.db.find_workspace(&asked.workspace).await? else {
        return Ok(());
    };
    let receipt = Receipt {
        request: asked.request.clone(),
        outcome,
    };
    end(shared, &place, &asked.chat, receipt, false).await
}

/// Ends a request to `chat` with `receipt`, a final receipt the server writes for the chat's
/// machine, and tells the chat's feeds; nothing when a receipt ended it already or, when
/// `refused_only`, it was sent again since its core refused it.
pub(crate) async fn end(
    shared: &Shared,
    workspace: &Workspace,
    chat: &ChatId,
    receipt: Receipt,
    refused_only: bool,
) -> Result<(), Fault> {
    if !workspace
        .end_open(chat, receipt, shared.now(), refused_only)
        .await?
    {
        return Ok(());
    }
    shared.wake_feeds(workspace.id());
    shared.chats.stored(workspace.id().clone(), chat.clone());
    Ok(())
}

async fn refused(shared: &Shared, asked: &Asked) -> Result<(), Fault> {
    let Some(place) = shared.db.find_workspace(&asked.workspace).await? else {
        return Ok(());
    };
    place.mark_refused(&asked.request).await
}

fn warn(shared: &Shared, machine: &MachineId, asked: &Asked, event: &'static str, fault: &Fault) {
    let fields = [
        ("machine_id", machine.as_str().into()),
        ("workspace_id", asked.workspace.as_str().into()),
        ("chat_id", asked.chat.as_str().into()),
        ("err", Field::error(fault)),
    ];
    shared.log.warn(event, &fields);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{NOW, command, id, make};
    use crate::test_support::Fixture;

    #[tokio::test]
    async fn a_refusal_that_cant_be_kept_is_logged_and_keeps_the_link() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.break_chat_requests().await;
        let sent = Sent::default();
        let machine = id::<MachineId>("mc-1");
        sent.sent(&machine, 7, &command("r-c-1", "c-1", "Fix the build"));
        let refused = Message::Error(ErrorBody {
            code: weacon_protocol::code!("chat.busy"),
            retryable: false,
        });

        let asked = sent.answered(&machine, 7).unwrap();
        take(shared, &machine, asked, refused).await.unwrap();

        let logged = crate::test_support::messages(&fixture.records);
        assert!(logged.contains(&"chat refusal not kept"), "{logged:?}");
    }

    #[tokio::test]
    async fn a_refusal_for_now_marks_its_command_refused_and_closes_the_link() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let sent = Sent::default();
        let machine = id::<MachineId>("mc-1");
        sent.sent(&machine, 7, &command("r-c-1", "c-1", "Fix the build"));
        let for_now = Message::Error(ErrorBody {
            code: weacon_protocol::code!("chat.unavailable"),
            retryable: true,
        });

        let asked = sent.answered(&machine, 7).unwrap();
        take(shared, &machine, asked, for_now).await.unwrap_err();

        let open = workspace.open_requests(&id("c-1")).await.unwrap();
        assert!(
            open.iter().all(|request| request.refused && !request.sent),
            "{open:?}"
        );
        assert_eq!(open.len(), 1);
    }

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
