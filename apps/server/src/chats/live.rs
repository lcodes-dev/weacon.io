//! What live viewers of a chat see besides its stored events, passed on and never stored: the
//! text its runs are writing, and the tool a permission prompt asks for.

use weacon_observability::Field;
use weacon_protocol::{
    ChatEntry, ChatEvent, ChatId, ChatLive, Code, MachineId, RunEvent, RunUpdate, WorkspaceId, code,
};

use crate::shared::Shared;

/// What the server offers to cores whose chats' live viewers it passes their updates to.
pub(crate) const LIVE: Code = code!("chat.live");

/// What a socket hears of a chat it may follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    pub(crate) news: News,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum News {
    /// Its events now reach `seq`; `shown` is the last as live viewers see it, when that isn't
    /// what is stored.
    Stored { seq: u64, shown: Option<ChatEntry> },
    /// A run's update, after the events stored so far.
    Run(RunUpdate),
}

/// `event` as live viewers see it, when that isn't what is stored: a permission prompt with
/// its tool's name, and never its input.
pub(super) fn shown(event: &ChatEvent) -> Option<ChatEvent> {
    let ChatEvent::Run { update } = event else {
        return None;
    };
    let RunEvent::PermissionAsked {
        run, prompt, tool, ..
    } = &update.event
    else {
        return None;
    };
    let event = RunEvent::PermissionAsked {
        run: run.clone(),
        prompt: *prompt,
        tool: tool.clone(),
        input: serde_json::Value::Null,
    };
    let update = RunUpdate {
        run: update.run.clone(),
        event,
    };
    Some(ChatEvent::Run { update })
}

/// Passes on what `machine` sent live of one of its chats: text being written, while the chat
/// runs there and its owner still owns it. Nothing else passes, and none of it is stored.
pub(crate) async fn pass(shared: &Shared, machine: &MachineId, live: ChatLive) {
    let ChatLive {
        workspace,
        chat,
        update,
    } = live;
    if !matches!(update.event, RunEvent::TextDelta { .. }) {
        return;
    }
    let ours = match shared.db.find_workspace(&workspace).await {
        Ok(Some(place)) => place.runs_on(&chat, machine).await,
        Ok(None) => Ok(false),
        Err(fault) => Err(fault),
    };
    match ours {
        Ok(true) => shared.chats.live(workspace, chat, update),
        Ok(false) => {}
        Err(fault) => {
            let fields = [
                ("machine_id", machine.as_str().into()),
                ("workspace_id", workspace.as_str().into()),
                ("chat_id", chat.as_str().into()),
                ("err", Field::error(&fault)),
            ];
            shared.log.warn("chat live update dropped", &fields);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::RunId;

    use super::*;
    use crate::chats::test_support::ada;
    use crate::db::test_support::{NOW, id, make, said};
    use crate::test_support::Fixture;

    fn run() -> RunId {
        id("run-1")
    }

    fn sent(workspace: &str, event: RunEvent) -> ChatLive {
        ChatLive {
            workspace: id(workspace),
            chat: id("c-1"),
            update: RunUpdate { run: run(), event },
        }
    }

    fn delta() -> RunEvent {
        RunEvent::TextDelta {
            text: "MARKER-DELTA".to_owned(),
        }
    }

    fn asked(tool: &str, input: serde_json::Value) -> ChatEvent {
        let event = RunEvent::PermissionAsked {
            run: run(),
            prompt: 1,
            tool: tool.to_owned(),
            input,
        };
        ChatEvent::Run {
            update: RunUpdate { run: run(), event },
        }
    }

    #[test]
    fn a_permission_prompt_is_shown_with_its_tools_name_and_never_its_input() {
        let asking = asked("Bash", json!({ "command": "MARKER-INPUT" }));

        assert_eq!(shown(&asking), Some(asked("Bash", serde_json::Value::Null)));
        assert_eq!(shown(&said("Hi")), None);
        assert_eq!(shown(&ChatEvent::Newer), None);
    }

    #[tokio::test]
    async fn only_text_being_written_in_a_chat_on_the_machine_that_sent_it_passes() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada(&fixture).await);
        make(&ada.member.workspace, "c-1", "m1", NOW).await.unwrap();
        ada.member.workspace.move_chats_to(&ada.machine).await;
        let mut notices = shared.chats.notices();

        pass(shared, &id("mc-2"), sent("w1", delta())).await;
        pass(shared, &ada.machine, sent("w-9", delta())).await;
        pass(shared, &ada.machine, sent("w1", RunEvent::Newer)).await;
        pass(shared, &ada.machine, sent("w1", delta())).await;

        let passed = Notice {
            workspace: id("w1"),
            chat: id("c-1"),
            news: News::Run(RunUpdate {
                run: run(),
                event: delta(),
            }),
        };
        assert_eq!(notices.try_recv().ok(), Some(passed));
        notices.try_recv().unwrap_err();
    }

    #[tokio::test]
    async fn text_that_cant_be_checked_is_dropped_and_logged_without_it() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada(&fixture).await);
        let mut notices = shared.chats.notices();
        shared.db.close().await;

        pass(shared, &ada.machine, sent("w1", delta())).await;

        notices.try_recv().unwrap_err();
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(logged.contains("chat live update dropped"), "{logged}");
        assert!(!logged.contains("MARKER"), "{logged}");
    }
}
