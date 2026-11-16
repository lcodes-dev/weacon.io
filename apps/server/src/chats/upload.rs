//! What a machine sends of the chats it runs: their events, stored in the order they come.

use serde_json::Value;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatEntry, ChatEvent, ChatId, ChatUpdate, Code, MachineId, RunEvent, RunUpdate, Wants,
    WorkspaceId, code,
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
        entry: ChatEntry { seq, event },
    } = update;
    let Some(event) = kept(event) else {
        return Ok(());
    };
    let fields = [
        ("machine_id", machine.as_str().into()),
        ("workspace_id", workspace.as_str().into()),
        ("chat_id", chat.as_str().into()),
    ];
    let incoming = Incoming {
        core_seq: Some(seq),
        event: &event,
        at: shared.now(),
    };
    match append(shared, machine, &workspace, &chat, incoming).await {
        Ok(true) => {
            // A turn's start or end changes the chat's summary in the workspace's list.
            if turns(&event) {
                shared.wake_feeds(&workspace);
            }
            shared.chats.stored(workspace, chat);
        }
        Ok(false) => shared.log.warn("chat upload refused", &fields),
        Err(fault) => {
            // A chat a limit just filled is news to the list.
            let full = [code!("chat.quota_full"), code!("chat.too_many_waiting")];
            if full.contains(fault.code()) {
                shared.wake_feeds(&workspace);
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

// The most bytes of a prompt's tool's name that are kept.
const TOOL_MAX: usize = 128;

fn kept_run(event: RunEvent) -> Option<RunEvent> {
    match event {
        RunEvent::Text { .. }
        | RunEvent::PermissionWithdrawn { .. }
        | RunEvent::PermissionAnswered { .. }
        | RunEvent::Finished { .. } => Some(event),
        // Kept with what it says the tool wants, never the tool's input.
        RunEvent::PermissionAsked {
            run,
            prompt,
            mut tool,
            wants,
            ..
        } => Some(RunEvent::PermissionAsked {
            run,
            prompt,
            tool: {
                tool.truncate(tool.floor_char_boundary(TOOL_MAX));
                tool
            },
            input: Value::Null,
            wants: wants.map(Wants::bounded),
        }),
        RunEvent::Started { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ToolCall { .. }
        | RunEvent::ToolDone { .. }
        | RunEvent::Newer => None,
    }
}

// Whether `incoming` was one to store, which it then is.
async fn append(
    shared: &Shared,
    machine: &MachineId,
    workspace: &WorkspaceId,
    chat: &ChatId,
    incoming: Incoming<'_>,
) -> Result<bool, Fault> {
    let Some(place) = shared.db.find_workspace(workspace).await? else {
        return Ok(false);
    };
    match place.append_from(machine, chat, &[incoming]).await {
        Err(fault) if *fault.code() == code!("chat.not_found") => Ok(false),
        appended => appended.map(|_| true),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatEntry, ChatEvent, Outcome, Receipt, WANTS_MAX, WorkspaceId};

    use super::*;
    use crate::chats::test_support::{Ada, ada};
    use crate::db::test_support::{NOW, append, asks, id, make, said};
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
        workspace.leave(&member_id("m1")).await.unwrap();
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
    async fn a_chat_too_many_prompts_fill_wakes_the_workspaces_feeds() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let waiting: Vec<_> = (1..=1000).map(|prompt| (None, asks(prompt))).collect();
        append(&ada.member.workspace, "c-1", &waiting)
            .await
            .unwrap();
        let mut feeds = shared.changes.subscribe();

        let one_more = update("w1", "c-1", 1, asks(1001));
        store(shared, &ada.machine, one_more).await.unwrap();

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
    }

    #[tokio::test]
    async fn messages_are_stored_and_a_prompts_tool_with_what_it_wants_but_no_tools_input() {
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
        let long = format!("MARKER{}", "é".repeat(WANTS_MAX));
        let asks = |tool: &str, input, text: &str| RunEvent::PermissionAsked {
            run: id("run-1"),
            prompt: 1,
            tool: tool.to_owned(),
            input,
            wants: Some(Wants {
                text: text.to_owned(),
                cut: false,
            }),
        };
        let asked = asks(&"é".repeat(TOOL_MAX), marked(), &long);

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
        // A core that sent more than a prompt may say has it cut here.
        let fits = format!("MARKER{}", "é".repeat(WANTS_MAX / 2 - 3));
        let mut kept = asks(&"é".repeat(TOOL_MAX / 2), Value::Null, &fits);
        if let RunEvent::PermissionAsked { wants, .. } = &mut kept {
            *wants = wants.take().map(|wants| Wants { cut: true, ..wants });
        }
        assert_eq!(stored, [run(kept), said("MARKER-TEXT")]);
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("chat upload refused"), "{logged}");
        assert!(!logged.contains("MARKER"), "{logged}");
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
