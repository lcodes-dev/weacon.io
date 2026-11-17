//! What a chat's viewers need of its runs as they happen, which the server passes on and never
//! keeps: text as it is written.

use std::collections::{HashMap, VecDeque};

use weacon_protocol::{
    ChatEvent, ChatLive, ChatUpdate, Envelope, Message, RunEvent, RunId, RunUpdate,
};

use crate::runs::ChatKey;

/// The most runs whose chat is remembered; the oldest is let go first.
const RUNS_MAX: usize = 1024;

/// The chats of the runs whose events went up, so their live updates can name their chat.
#[derive(Debug, Default)]
pub(super) struct Live {
    runs: HashMap<RunId, ChatKey>,
    order: VecDeque<RunId>,
}

impl Live {
    /// Takes note of the run `update` is about.
    pub(super) fn saw(&mut self, update: &ChatUpdate) {
        let run = match &update.entry.event {
            ChatEvent::RunStatus { status } => &status.run,
            ChatEvent::Run { update: run } => &run.run,
            ChatEvent::Message { .. }
            | ChatEvent::Prompt { .. }
            | ChatEvent::OutsidePrompt { .. }
            | ChatEvent::Receipt { .. } => return,
        };
        self.remember(run, &ChatKey::new(&update.workspace, &update.chat));
    }

    /// `pushed`, a run's update, as it goes up live: text as it is written, after `came`, the
    /// last event of its chat that went up, and nothing else.
    pub(super) fn of(&self, pushed: RunUpdate, came: &HashMap<ChatKey, u64>) -> Option<Envelope> {
        if !matches!(pushed.event, RunEvent::TextDelta { .. }) {
            return None;
        }
        let key = self.runs.get(&pushed.run)?;
        let live = ChatLive {
            workspace: key.workspace.clone(),
            chat: key.chat.clone(),
            update: pushed,
            after: Some(came.get(key).copied().unwrap_or_default()),
        };
        Some(Envelope {
            id: 0,
            message: Message::ChatLive(live),
        })
    }

    fn remember(&mut self, run: &RunId, key: &ChatKey) {
        if self.runs.insert(run.clone(), key.clone()).is_some() {
            return;
        }
        self.order.push_back(run.clone());
        if self.order.len() > RUNS_MAX
            && let Some(oldest) = self.order.pop_front()
        {
            self.runs.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
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

    fn delta(run: &str) -> RunUpdate {
        RunUpdate {
            run: id(run),
            event: RunEvent::TextDelta {
                text: "Wor".to_owned(),
            },
        }
    }

    fn up(chat: &str, run: &str, event: RunEvent, after: u64) -> Envelope {
        let live = ChatLive {
            workspace: id("w1"),
            chat: id(chat),
            update: RunUpdate {
                run: id(run),
                event,
            },
            after: Some(after),
        };
        Envelope {
            id: 0,
            message: Message::ChatLive(live),
        }
    }

    #[test]
    fn text_being_written_goes_up_live_named_by_its_runs_chat_and_nothing_else() {
        let mut live = Live::default();
        let came = HashMap::from([(ChatKey::new(&id("w1"), &id("c-1")), 7)]);
        assert_eq!(live.of(delta("run-1"), &came), None);

        live.saw(&update("c-1", started("run-1")));
        let said = RunUpdate {
            run: id("run-2"),
            event: RunEvent::Text {
                text: "Done".to_owned(),
            },
        };
        assert_eq!(live.of(delta("run-2"), &came), None);
        live.saw(&update(
            "c-2",
            ChatEvent::Run {
                update: said.clone(),
            },
        ));

        assert_eq!(
            live.of(delta("run-1"), &came),
            Some(up("c-1", "run-1", delta("run-1").event, 7))
        );
        // A chat none of whose events went up yet is after nothing.
        assert_eq!(
            live.of(delta("run-2"), &came),
            Some(up("c-2", "run-2", delta("run-2").event, 0))
        );
        assert_eq!(live.of(said, &came), None);
    }

    #[test]
    fn old_runs_are_let_go() {
        let mut live = Live::default();
        for n in 0..=RUNS_MAX {
            live.saw(&update("c-1", started(&format!("run-{n}"))));
        }
        assert_eq!(live.of(delta("run-0"), &HashMap::new()), None);
        assert!(live.of(delta("run-1"), &HashMap::new()).is_some());
        live.saw(&update("c-1", started("run-1")));
        assert_eq!(live.order.len(), RUNS_MAX);
    }
}
