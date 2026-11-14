//! What a machine sends of the chats it runs: their events, stored in the order they come.

use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatUpdate, MachineId};

use crate::db::{Incoming, turns};
use crate::http::Shared;

/// Stores `update` from `machine`, and tells the sockets that follow its chat. An event of a
/// chat that isn't on `machine`, in any workspace, is dropped.
pub(crate) async fn chat(shared: &Shared, machine: &MachineId, update: ChatUpdate) {
    let ChatUpdate {
        workspace,
        chat,
        entry,
    } = update;
    let stored = async {
        let Some(place) = shared.db.find_workspace(&workspace).await? else {
            return Ok(false);
        };
        let Some(found) = place.chat(&chat).await? else {
            return Ok(false);
        };
        if found.machine != *machine {
            return Ok(false);
        }
        let incoming = Incoming {
            core_seq: Some(entry.seq),
            event: &entry.event,
            at: shared.now(),
        };
        place.append_events(&chat, &[incoming]).await?;
        Ok::<_, Fault>(true)
    };
    let fields = [
        ("machine_id", machine.as_str().into()),
        ("workspace_id", workspace.as_str().into()),
        ("chat_id", chat.as_str().into()),
    ];
    match stored.await {
        Ok(true) => {
            // A turn's start or end changes the chat's summary in the workspace's list.
            if turns(&entry.event) {
                shared.changed(&workspace);
            }
            shared.chats.stored(workspace, chat);
        }
        Ok(false) => shared.log.warn("chat upload refused", &fields),
        Err(fault) => {
            let [machine, workspace, chat] = fields;
            let fields = [machine, workspace, chat, ("err", Field::error(&fault))];
            shared.log.warn("chat upload failed", &fields);
        }
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{
        ChatEntry, ChatEvent, Outcome, Receipt, RunEvent, RunId, RunUpdate, WorkspaceId,
    };

    use super::*;
    use crate::db::test_support::{NOW, id, make};
    use crate::test_support::Fixture;

    fn update(workspace: &str, chat: &str, seq: u64, text: &str) -> ChatUpdate {
        let event = ChatEvent::Run {
            update: RunUpdate {
                run: id::<RunId>("run-1"),
                event: RunEvent::Text {
                    text: text.to_owned(),
                },
            },
        };
        ChatUpdate {
            workspace: id(workspace),
            chat: id(chat),
            entry: ChatEntry { seq, event },
        }
    }

    #[tokio::test]
    async fn only_the_chats_own_machine_adds_to_it_and_its_followers_hear_of_it() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id::<WorkspaceId>("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let mut notices = shared.chats.notices();
        let (ours, other) = (id::<MachineId>("mc-1"), id::<MachineId>("mc-2"));

        chat(shared, &ours, update("w1", "c-1", 4, "MARKER-TEXT")).await;
        chat(shared, &other, update("w1", "c-1", 5, "MARKER-TEXT")).await;
        chat(shared, &ours, update("w1", "c-9", 6, "MARKER-TEXT")).await;
        chat(shared, &ours, update("w-9", "c-1", 7, "MARKER-TEXT")).await;

        let events = workspace.events_after(&id("c-1"), 0, 10, usize::MAX).await;
        assert_eq!(events.unwrap().len(), 1);
        assert_eq!(
            workspace.chat(&id("c-1")).await.unwrap().unwrap().core_seq,
            4
        );
        assert_eq!(notices.try_recv().unwrap(), (id("w1"), id("c-1")));
        notices.try_recv().unwrap_err();
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert_eq!(logged.matches("chat upload refused").count(), 3);
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_turn_ending_wakes_the_workspaces_feeds_and_text_does_not() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let workspace = shared.db.workspace(&id::<WorkspaceId>("w1")).await.unwrap();
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let mut feeds = shared.changes.subscribe();
        let machine = id::<MachineId>("mc-1");

        chat(shared, &machine, update("w1", "c-1", 1, "working")).await;
        feeds.try_recv().unwrap_err();
        let receipt = Receipt {
            request: id("r-c-1"),
            outcome: Outcome::Completed,
        };
        let ended = ChatUpdate {
            workspace: id("w1"),
            chat: id("c-1"),
            entry: ChatEntry {
                seq: 2,
                event: ChatEvent::Receipt { receipt },
            },
        };
        chat(shared, &machine, ended).await;

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
    }
}
