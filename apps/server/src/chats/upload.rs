//! What a machine sends of the chats it runs: their events, stored in the order they come.

use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatUpdate, Code, MachineId, code};

use crate::db::{Incoming, turns};
use crate::shared::Shared;

/// What the server offers to cores whose chats' events it stores.
pub(crate) const UPLOAD: Code = code!("chat.upload");

/// Stores `update` from `machine`, and tells the sockets that follow its chat. An event of a
/// chat that isn't on `machine`, or whose owner no longer owns it there, is dropped.
pub(crate) async fn store(shared: &Shared, machine: &MachineId, update: ChatUpdate) {
    let fields = [
        ("machine_id", machine.as_str().into()),
        ("workspace_id", update.workspace.as_str().into()),
        ("chat_id", update.chat.as_str().into()),
    ];
    match append(shared, machine, &update).await {
        Ok(true) => {
            // A turn's start or end changes the chat's summary in the workspace's list.
            if turns(&update.entry.event) {
                shared.wake_feeds(&update.workspace);
            }
            shared.chats.stored(update.workspace, update.chat);
        }
        Ok(false) => shared.log.warn("chat upload refused", &fields),
        Err(fault) => {
            // A chat the quota just filled is news to the list.
            if *fault.code() == code!("chat.quota_full") {
                shared.wake_feeds(&update.workspace);
            }
            let fields = [&fields[..], &[("err", Field::error(&fault))]].concat();
            shared.log.warn("chat upload failed", &fields);
        }
    }
}

// Whether `update` was one to store, which it then is.
async fn append(shared: &Shared, machine: &MachineId, update: &ChatUpdate) -> Result<bool, Fault> {
    let ChatUpdate {
        workspace,
        chat,
        entry,
    } = update;
    let Some(place) = shared.db.find_workspace(workspace).await? else {
        return Ok(false);
    };
    if !place.runs_on(chat, machine).await? {
        return Ok(false);
    }
    let incoming = Incoming {
        core_seq: Some(entry.seq),
        event: &entry.event,
        at: shared.now(),
    };
    place.append_events(chat, &[incoming]).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatEntry, ChatEvent, Outcome, Receipt, WorkspaceId};

    use super::*;
    use crate::chats::test_support::{Ada, ada};
    use crate::db::test_support::{NOW, id, make, said};
    use crate::test_support::{Fixture, member_id};

    fn update(workspace: &str, chat: &str, seq: u64, event: ChatEvent) -> ChatUpdate {
        ChatUpdate {
            workspace: id(workspace),
            chat: id(chat),
            entry: ChatEntry { seq, event },
        }
    }

    // Ada, with chat c-1 in w1 on her machine.
    async fn ada_chatting(fixture: &Fixture) -> Ada {
        let ada = ada(fixture).await;
        make(&ada.member.workspace, "c-1", "m1", NOW).await.unwrap();
        ada.member.workspace.move_chats_to(&ada.machine).await;
        ada
    }

    #[tokio::test]
    async fn only_the_chats_own_machine_adds_to_it_and_its_followers_hear_of_it() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let (workspace, ours) = (&ada.member.workspace, &ada.machine);
        let mut notices = shared.chats.notices();
        let other = id::<MachineId>("mc-2");

        store(shared, ours, update("w1", "c-1", 4, said("MARKER-TEXT"))).await;
        store(shared, &other, update("w1", "c-1", 5, said("MARKER-TEXT"))).await;
        store(shared, ours, update("w1", "c-9", 6, said("MARKER-TEXT"))).await;
        store(shared, ours, update("w-9", "c-1", 7, said("MARKER-TEXT"))).await;
        workspace.unlink(&member_id("m1")).await;
        store(shared, ours, update("w1", "c-1", 8, said("MARKER-TEXT"))).await;

        let events = workspace.events_after(&id("c-1"), 0, 10, usize::MAX).await;
        assert_eq!(events.unwrap().len(), 1);
        let core_seq = workspace.chat(&id("c-1")).await.unwrap().unwrap().core_seq;
        assert_eq!(core_seq, 4);
        assert_eq!(notices.try_recv().unwrap(), (id("w1"), id("c-1")));
        notices.try_recv().unwrap_err();
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert_eq!(logged.matches("chat upload refused").count(), 4);
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_turn_ending_wakes_the_workspaces_feeds_and_text_does_not() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let mut feeds = shared.changes.subscribe();

        store(
            shared,
            &ada.machine,
            update("w1", "c-1", 1, said("working")),
        )
        .await;
        feeds.try_recv().unwrap_err();
        let receipt = Receipt {
            request: id("r-c-1"),
            outcome: Outcome::Completed,
        };
        let ended = update("w1", "c-1", 2, ChatEvent::Receipt { receipt });
        store(shared, &ada.machine, ended).await;

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
    }

    #[tokio::test]
    async fn a_chat_the_quota_fills_wakes_the_workspaces_feeds() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        ada.member.workspace.fill_quota().await;
        let mut feeds = shared.changes.subscribe();

        store(shared, &ada.machine, update("w1", "c-1", 1, said("a"))).await;

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
        let logged = crate::test_support::messages(&fixture.records);
        assert!(logged.contains(&"chat upload failed"), "{logged:?}");
    }
}
