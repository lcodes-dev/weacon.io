//! What a machine sends of the chats it runs: their events, stored in the order they come.

use serde_json::Value;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatEntry, ChatEvent, ChatUpdate, Code, MachineId, RunEvent, RunUpdate, code,
};

use crate::db::{Incoming, turns};
use crate::shared::Shared;
use crate::socket::Closed;

/// What the server offers to cores whose chats' events it stores.
pub(crate) const UPLOAD: Code = code!("chat.upload");

/// Stores what is [`kept`] of `update` from `machine`, and tells the sockets that follow its
/// chat. An event of a chat that isn't on `machine`, or whose owner no longer owns it there, is
/// dropped. When the database fails, the link closes, so nothing later is stored before it
/// and the core, connecting again, sends it once more.
pub(crate) async fn store(
    shared: &Shared,
    machine: &MachineId,
    update: ChatUpdate,
) -> Result<(), Closed> {
    let ChatUpdate {
        workspace,
        chat,
        entry,
    } = update;
    let Some(event) = kept(entry.event) else {
        return Ok(());
    };
    let seq = entry.seq;
    let update = ChatUpdate {
        workspace,
        chat,
        entry: ChatEntry { seq, event },
    };
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
            if *fault.code() == code!("db.failed") {
                return Err(Closed);
            }
        }
    }
    Ok(())
}

/// What the server keeps of a chat event: its messages, never a tool's name or input.
fn kept(event: ChatEvent) -> Option<ChatEvent> {
    match event {
        ChatEvent::Run { update } => {
            let RunUpdate { run, event } = update;
            let event = kept_run(event)?;
            Some(ChatEvent::Run {
                update: RunUpdate { run, event },
            })
        }
        ChatEvent::Prompt { .. }
        | ChatEvent::OutsidePrompt { .. }
        | ChatEvent::Receipt { .. }
        | ChatEvent::RunStatus { .. } => Some(event),
        // What a newer core sends may hold anything.
        ChatEvent::Newer => None,
    }
}

fn kept_run(event: RunEvent) -> Option<RunEvent> {
    match event {
        RunEvent::Text { .. }
        | RunEvent::PermissionWithdrawn { .. }
        | RunEvent::PermissionAnswered { .. }
        | RunEvent::Finished { .. } => Some(event),
        // Kept so it can be answered, without what the tool would do.
        RunEvent::PermissionAsked { run, prompt, .. } => Some(RunEvent::PermissionAsked {
            run,
            prompt,
            tool: String::new(),
            input: Value::Null,
        }),
        RunEvent::Started { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ToolCall { .. }
        | RunEvent::ToolDone { .. }
        | RunEvent::Newer => None,
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
    let incoming = Incoming {
        core_seq: Some(entry.seq),
        event: &entry.event,
        at: shared.now(),
    };
    match place.append_from(machine, chat, &[incoming]).await {
        Err(fault) if *fault.code() == code!("chat.not_found") => Ok(false),
        appended => appended.map(drop).map(|()| true),
    }
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

        store(shared, ours, update("w1", "c-1", 4, said("MARKER-TEXT")))
            .await
            .unwrap();
        store(shared, &other, update("w1", "c-1", 5, said("MARKER-TEXT")))
            .await
            .unwrap();
        store(shared, ours, update("w1", "c-9", 6, said("MARKER-TEXT")))
            .await
            .unwrap();
        store(shared, ours, update("w-9", "c-1", 7, said("MARKER-TEXT")))
            .await
            .unwrap();
        workspace.unlink(&member_id("m1")).await;
        store(shared, ours, update("w1", "c-1", 8, said("MARKER-TEXT")))
            .await
            .unwrap();

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
        .await
        .unwrap();
        feeds.try_recv().unwrap_err();
        let receipt = Receipt {
            request: id("r-c-1"),
            outcome: Outcome::Completed,
        };
        let ended = update("w1", "c-1", 2, ChatEvent::Receipt { receipt });
        store(shared, &ada.machine, ended).await.unwrap();

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
    }

    #[tokio::test]
    async fn a_chat_the_quota_fills_wakes_the_workspaces_feeds() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        ada.member.workspace.fill_quota().await;
        let mut feeds = shared.changes.subscribe();

        store(shared, &ada.machine, update("w1", "c-1", 1, said("a")))
            .await
            .unwrap();

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
        let logged = crate::test_support::messages(&fixture.records);
        assert!(logged.contains(&"chat upload failed"), "{logged:?}");
    }

    #[tokio::test]
    async fn a_failed_store_closes_the_link_and_keeps_the_chats_place() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let workspace = &ada.member.workspace;
        store(shared, &ada.machine, update("w1", "c-1", 1, said("a")))
            .await
            .unwrap();
        workspace.break_chat_events().await;

        let failed = store(shared, &ada.machine, update("w1", "c-1", 2, said("b"))).await;

        failed.unwrap_err();
        let chat = workspace.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!(chat.core_seq, 1);
        // Refused for good, not failed: the link goes on.
        workspace.fill_quota().await;
        store(shared, &ada.machine, update("w1", "c-1", 3, said("c")))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_unpaired_machine_stores_nothing_in_the_chats_it_ran() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let (workspace, chat) = (&ada.member.workspace, id("c-1"));
        let event = said("a");
        let incoming = || Incoming {
            core_seq: Some(1),
            event: &event,
            at: NOW,
        };
        let elsewhere = workspace
            .append_from(&id("mc-2"), &chat, &[incoming()])
            .await;
        assert_eq!(elsewhere.unwrap_err().code(), &code!("chat.not_found"));
        let unpaired = shared.db.unpair(&ada.asker.account, &ada.machine).await;
        assert!(unpaired.unwrap());

        let stored = workspace
            .append_from(&ada.machine, &chat, &[incoming()])
            .await;

        assert_eq!(stored.unwrap_err().code(), &code!("chat.not_found"));
        let events = workspace.events_after(&chat, 0, 10, usize::MAX).await;
        assert!(events.unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_messages_are_stored_and_no_tools_name_or_input() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let workspace = &ada.member.workspace;
        let run = |event| ChatEvent::Run {
            update: RunUpdate {
                run: id("run-1"),
                event,
            },
        };
        let marked = || serde_json::json!({ "path": "MARKER-INPUT" });
        let call = RunEvent::ToolCall {
            call: "call-1".to_owned(),
            tool: "MARKER-TOOL".to_owned(),
            input: marked(),
        };
        let asked = RunEvent::PermissionAsked {
            run: id("run-1"),
            prompt: 1,
            tool: "MARKER-TOOL".to_owned(),
            input: marked(),
        };

        for (seq, event) in [(1, run(call)), (2, run(asked)), (3, said("MARKER-TEXT"))] {
            store(shared, &ada.machine, update("w1", "c-1", seq, event))
                .await
                .unwrap();
        }

        let events = workspace.events_after(&id("c-1"), 0, 10, usize::MAX).await;
        let stored: Vec<_> = events
            .unwrap()
            .into_iter()
            .map(|entry| entry.event)
            .collect();
        let blank = RunEvent::PermissionAsked {
            run: id("run-1"),
            prompt: 1,
            tool: String::new(),
            input: Value::Null,
        };
        assert_eq!(stored, [run(blank), said("MARKER-TEXT")]);
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("chat upload refused"), "{logged}");
    }

    #[test]
    fn what_a_newer_core_sends_or_a_tool_does_is_not_kept() {
        let run = |event| ChatEvent::Run {
            update: RunUpdate {
                run: id("run-1"),
                event,
            },
        };
        let done = RunEvent::ToolDone {
            call: "call-1".to_owned(),
            failed: false,
        };
        let finished = RunEvent::Finished {
            outcome: Outcome::Completed,
        };
        assert_eq!(kept(ChatEvent::Newer), None);
        assert_eq!(kept(run(RunEvent::Newer)), None);
        assert_eq!(kept(run(done)), None);
        assert_eq!(kept(run(finished.clone())), Some(run(finished)));
        let outside = ChatEvent::OutsidePrompt {
            text: "Go".to_owned(),
        };
        assert_eq!(kept(outside.clone()), Some(outside));
    }
}
