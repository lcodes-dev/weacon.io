//! What a chat's viewers need of its runs as they happen, which the server passes on and never
//! keeps: text as it is written, and the tool a permission prompt names.

use std::collections::HashMap;

use weacon_protocol::{
    ChatEvent, ChatLive, ChatUpdate, Code, Envelope, Message, RunEvent, RunId, RunUpdate, code,
};

use crate::runs::ChatKey;

/// What a server offers when it passes a chat's live updates on to its viewers.
pub(in crate::uplink) const LIVE: Code = code!("chat.live");
/// The most runs whose chat is remembered; the oldest is let go first.
const RUNS_MAX: usize = 1024;

/// The chats of the runs whose events went up, so their live updates can name their chat.
#[derive(Debug, Default)]
pub(super) struct Live {
    on: bool,
    runs: HashMap<RunId, ChatKey>,
    order: Vec<RunId>,
}

impl Live {
    /// Sends live updates up only when `on`, as the server offered [`LIVE`].
    pub(super) fn new(on: bool) -> Self {
        Self {
            on,
            ..Self::default()
        }
    }

    /// Takes note of the run `update` is about, and gives what goes up live beside it: the
    /// tool a permission prompt names, which the stored event leaves out.
    pub(super) fn saw(&mut self, update: &ChatUpdate) -> Option<Envelope> {
        let key = ChatKey::new(&update.workspace, &update.chat);
        let (run, asked) = match &update.entry.event {
            ChatEvent::RunStatus { status } => (&status.run, None),
            ChatEvent::Run { update: run } => (&run.run, named(&run.event)),
            ChatEvent::Prompt { .. }
            | ChatEvent::OutsidePrompt { .. }
            | ChatEvent::Receipt { .. }
            | ChatEvent::Newer => return None,
        };
        self.remember(run, &key);
        let event = asked?;
        self.up(&key, run.clone(), event)
    }

    /// `pushed`, a run's update, as it goes up live: text as it is written, and nothing else.
    pub(super) fn of(&self, pushed: RunUpdate) -> Option<Envelope> {
        if !matches!(pushed.event, RunEvent::TextDelta { .. }) {
            return None;
        }
        let key = self.runs.get(&pushed.run)?.clone();
        self.up(&key, pushed.run, pushed.event)
    }

    fn up(&self, key: &ChatKey, run: RunId, event: RunEvent) -> Option<Envelope> {
        let live = ChatLive {
            workspace: key.workspace.clone(),
            chat: key.chat.clone(),
            update: RunUpdate { run, event },
        };
        self.on.then_some(Envelope {
            id: 0,
            message: Message::ChatLive(live),
        })
    }

    fn remember(&mut self, run: &RunId, key: &ChatKey) {
        if self.runs.insert(run.clone(), key.clone()).is_some() {
            return;
        }
        self.order.push(run.clone());
        if self.order.len() > RUNS_MAX {
            let oldest = self.order.remove(0);
            self.runs.remove(&oldest);
        }
    }
}

// A permission prompt's event with only what a viewer needs: its tool's name, never its input.
fn named(event: &RunEvent) -> Option<RunEvent> {
    let RunEvent::PermissionAsked {
        run, prompt, tool, ..
    } = event
    else {
        return None;
    };
    Some(RunEvent::PermissionAsked {
        run: run.clone(),
        prompt: *prompt,
        tool: tool.clone(),
        input: serde_json::Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{ChatEntry, RunState, RunStatus};

    use super::*;

    fn id<T: TryFrom<String>>(text: &str) -> T {
        T::try_from(text.to_owned()).ok().unwrap()
    }

    fn update(chat: &str, event: ChatEvent) -> ChatUpdate {
        ChatUpdate {
            workspace: id("w1"),
            chat: id(chat),
            entry: ChatEntry { seq: 1, event },
        }
    }

    fn started(run: &str) -> ChatEvent {
        let status = RunStatus {
            run: id(run),
            state: RunState::Running,
            agent: None,
        };
        ChatEvent::RunStatus { status }
    }

    fn ask(tool: &str, input: serde_json::Value) -> RunEvent {
        RunEvent::PermissionAsked {
            run: id("run-1"),
            prompt: 1,
            tool: tool.to_owned(),
            input,
        }
    }

    fn delta(run: &str) -> RunUpdate {
        RunUpdate {
            run: id(run),
            event: RunEvent::TextDelta {
                text: "Wor".to_owned(),
            },
        }
    }

    fn up(chat: &str, event: RunEvent) -> Envelope {
        let live = ChatLive {
            workspace: id("w1"),
            chat: id(chat),
            update: RunUpdate {
                run: id("run-1"),
                event,
            },
        };
        Envelope {
            id: 0,
            message: Message::ChatLive(live),
        }
    }

    #[test]
    fn text_being_written_and_a_prompts_tool_go_up_live_named_by_their_chat_and_nothing_else() {
        let mut live = Live::new(true);
        assert_eq!(live.of(delta("run-1")), None);

        assert_eq!(live.saw(&update("c-1", started("run-1"))), None);
        let asked = ChatEvent::Run {
            update: RunUpdate {
                run: id("run-1"),
                event: ask("Bash", json!({ "command": "MARKER" })),
            },
        };

        assert_eq!(
            live.of(delta("run-1")),
            Some(up("c-1", delta("run-1").event))
        );
        assert_eq!(live.of(delta("run-2")), None);
        let said = RunUpdate {
            run: id("run-1"),
            event: RunEvent::Text {
                text: "Done".to_owned(),
            },
        };
        assert_eq!(live.of(said), None);
        let beside = live.saw(&update("c-1", asked));
        assert_eq!(
            beside,
            Some(up("c-1", ask("Bash", serde_json::Value::Null)))
        );
    }

    #[test]
    fn nothing_goes_up_live_to_a_server_that_doesnt_pass_it_on_and_old_runs_are_let_go() {
        let mut off = Live::new(false);
        off.saw(&update("c-1", started("run-1")));
        assert_eq!(off.of(delta("run-1")), None);

        let mut live = Live::new(true);
        for n in 0..=RUNS_MAX {
            live.saw(&update("c-1", started(&format!("run-{n}"))));
        }
        assert_eq!(live.of(delta("run-0")), None);
        assert!(live.of(delta("run-1")).is_some());
        live.saw(&update("c-1", started("run-1")));
        assert_eq!(live.order.len(), RUNS_MAX);
    }
}
