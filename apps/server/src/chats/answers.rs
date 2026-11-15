//! What a core answers under a chat command's envelope: a refusal, which ends the command.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatCommand, ChatId, ErrorBody, MachineId, Message, Outcome, Receipt, RequestId, WorkspaceId,
};

use crate::db::{Guard, Workspace};
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

/// The command an envelope carried, and the send attempt of its request it was.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    workspace: WorkspaceId,
    chat: ChatId,
    request: RequestId,
    attempt: i64,
}

impl Sent {
    /// Remembers that the envelope `id` carried `command` to `machine`, as its `attempt`.
    pub(crate) fn sent(&self, machine: &MachineId, id: u32, command: &ChatCommand, attempt: i64) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (machine.clone(), id);
        let asked = Asked {
            workspace: command.workspace.clone(),
            chat: command.chat.clone(),
            request: command.request.clone(),
            attempt,
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

/// What a core's `answer` to the chat command `asked` makes of it, unless a later attempt of its
/// request was queued since. A refusal for now marks the request refused and closes the link,
/// so it is sent again once the core connects again; any other is the command's final receipt,
/// stored in its chat for its feeds to hear.
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
            return match refused(shared, &asked).await {
                Ok(true) => Err(Closed),
                // A later attempt was queued since: its answer counts.
                Ok(false) => Ok(()),
                Err(fault) => {
                    warn(shared, machine, &asked, "chat refusal not marked", &fault);
                    Err(Closed)
                }
            };
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
    let guard = Guard {
        attempt: asked.attempt,
        refused_only: false,
    };
    end(shared, &place, &asked.chat, receipt, guard).await
}

/// Ends a request to `chat` with `receipt`, a final receipt the server writes for the chat's
/// machine, and tells the chat's feeds; nothing unless it is open and `guard` holds.
pub(crate) async fn end(
    shared: &Shared,
    workspace: &Workspace,
    chat: &ChatId,
    receipt: Receipt,
    guard: Guard,
) -> Result<(), Fault> {
    if !workspace
        .end_open(chat, receipt, shared.now(), guard)
        .await?
    {
        return Ok(());
    }
    shared.wake_feeds(workspace.id());
    shared.chats.stored(workspace.id().clone(), chat.clone());
    Ok(())
}

// Whether the refusal of `asked` counts, as no later attempt of its request was queued since.
async fn refused(shared: &Shared, asked: &Asked) -> Result<bool, Fault> {
    let Some(place) = shared.db.find_workspace(&asked.workspace).await? else {
        return Ok(false);
    };
    place.mark_refused(&asked.request, asked.attempt).await
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
    async fn a_refusal_of_an_attempt_sent_again_since_changes_nothing_and_keeps_the_link() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let sent = Sent::default();
        let machine = id::<MachineId>("mc-1");
        let request = id::<RequestId>("r-c-1");
        let asked = command("r-c-1", "c-1", "Fix the build");
        let first = workspace
            .begin_attempt(&request, NOW)
            .await
            .unwrap()
            .unwrap();
        sent.sent(&machine, 7, &asked, first.number);
        let for_now = || {
            Message::Error(ErrorBody {
                code: weacon_protocol::code!("chat.unavailable"),
                retryable: true,
            })
        };

        // Its refusal is matched, then a retry is queued before the refusal is kept.
        let older = sent.answered(&machine, 7).unwrap();
        let second = workspace
            .begin_attempt(&request, NOW)
            .await
            .unwrap()
            .unwrap();
        workspace.queued(&request, second).await.unwrap();
        take(shared, &machine, older, for_now()).await.unwrap();

        let open = workspace.open_requests(&id("c-1")).await.unwrap();
        assert!(open.iter().all(|request| !request.refused), "{open:?}");
        let later = workspace.refused_before(&id("c-1"), &id("r-2")).await;
        assert!(!later.unwrap());
        sent.sent(&machine, 8, &asked, second.number);
        let newer = sent.answered(&machine, 8).unwrap();
        take(shared, &machine, newer, for_now()).await.unwrap_err();
        let open = workspace.open_requests(&id("c-1")).await.unwrap();
        assert!(open.iter().all(|request| request.refused), "{open:?}");
    }

    #[tokio::test]
    async fn a_refusal_heard_while_a_later_send_waits_counts_and_stays_once_it_never_goes_out() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let sent = Sent::default();
        let machine = id::<MachineId>("mc-1");
        let request = id::<RequestId>("r-c-1");
        let first = workspace.begin_attempt(&request, NOW).await.unwrap();
        sent.sent(&machine, 7, &command("r-c-1", "c-1", "Fix the build"), 1);
        let second = workspace.begin_attempt(&request, NOW).await.unwrap();
        let for_now = Message::Error(ErrorBody {
            code: weacon_protocol::code!("chat.unavailable"),
            retryable: true,
        });

        let asked = sent.answered(&machine, 7).unwrap();
        take(shared, &machine, asked, for_now).await.unwrap_err();
        workspace.abandon(&request, second.unwrap()).await.unwrap();

        let open = workspace.open_requests(&id("c-1")).await.unwrap();
        assert_eq!(first.map(|attempt| attempt.number), Some(1));
        assert!(open.iter().all(|request| request.refused), "{open:?}");
        assert!(
            workspace
                .refused_before(&id("c-1"), &id("r-2"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_final_refusal_ends_its_command_once_and_tells_the_feeds_once() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let mut feeds = shared.changes.subscribe();
        let sent = Sent::default();
        let machine = id::<MachineId>("mc-1");
        let busy = || {
            Message::Error(ErrorBody {
                code: weacon_protocol::code!("chat.busy"),
                retryable: false,
            })
        };

        let mut woken = Vec::new();
        for envelope in [7, 8] {
            sent.sent(
                &machine,
                envelope,
                &command("r-c-1", "c-1", "Fix the build"),
                0,
            );
            let asked = sent.answered(&machine, envelope).unwrap();
            take(shared, &machine, asked, busy()).await.unwrap();
            woken.push(feeds.try_recv().ok());
        }

        assert!(
            workspace
                .open_requests(&id("c-1"))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(woken, [Some(id::<WorkspaceId>("w1")), None]);
    }

    #[tokio::test]
    async fn a_refusal_that_cant_be_kept_is_logged_and_keeps_the_link() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.break_chat_requests().await;
        let sent = Sent::default();
        let machine = id::<MachineId>("mc-1");
        sent.sent(&machine, 7, &command("r-c-1", "c-1", "Fix the build"), 0);
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
        sent.sent(&machine, 7, &command("r-c-1", "c-1", "Fix the build"), 0);
        let for_now = Message::Error(ErrorBody {
            code: weacon_protocol::code!("chat.unavailable"),
            retryable: true,
        });

        let asked = sent.answered(&machine, 7).unwrap();
        take(shared, &machine, asked, for_now).await.unwrap_err();

        let open = workspace.open_requests(&id("c-1")).await.unwrap();
        assert!(open.iter().all(|request| request.refused), "{open:?}");
        assert_eq!(open.len(), 1);
    }

    #[test]
    fn a_command_is_answered_once_on_its_own_machine_and_the_oldest_are_let_go() {
        let sent = Sent::default();
        let (ours, theirs) = (id::<MachineId>("mc-1"), id::<MachineId>("mc-2"));
        let asked = command("r-1", "c-1", "Go");
        sent.sent(&ours, 1, &asked, 3);

        assert_eq!(sent.answered(&theirs, 1), None);
        let answered = sent.answered(&ours, 1).unwrap();
        assert_eq!(answered.request.as_str(), "r-1");
        assert_eq!(answered.chat.as_str(), "c-1");
        assert_eq!(answered.attempt, 3);
        assert_eq!(sent.answered(&ours, 1), None);
        for envelope in 0..=u32::try_from(KEPT).unwrap() {
            sent.sent(&ours, envelope, &asked, 1);
        }
        assert_eq!(sent.answered(&ours, 0), None);
        assert!(sent.answered(&ours, 1).is_some());
        assert!(sent.answered(&ours, u32::try_from(KEPT).unwrap()).is_some());
    }
}
