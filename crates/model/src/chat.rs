//! What a chat's log says now: where its latest run stands and which permission prompts wait
//! for a person.

use weacon_protocol::{ChatEntry, ChatEvent, RunEvent, RunId, RunState, RunStatus, RunUpdate};

/// The state a chat's log folds into, one entry at a time, in log order from its first entry.
/// Folding a window that starts later gives the state of only what the window holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatState {
    latest: Option<RunStatus>,
    /// Each prompt still waiting: the run that asked, its number and the place of its entry.
    waiting: Vec<(RunId, u32, u64)>,
}

impl ChatState {
    /// The state `entries` fold into.
    #[must_use]
    pub fn fold(entries: &[ChatEntry]) -> Self {
        let mut state = Self::default();
        for entry in entries {
            state.apply(entry);
        }
        state
    }

    /// Takes in the log's next entry.
    pub fn apply(&mut self, entry: &ChatEntry) {
        match &entry.event {
            ChatEvent::RunStatus { status } => self.status(status),
            ChatEvent::Run {
                update: RunUpdate { run, event },
            } => self.said(run, event, entry.seq),
            ChatEvent::Prompt { .. } | ChatEvent::Receipt { .. } | ChatEvent::Newer => {}
        }
    }

    /// The latest run, as its last status says; a chat runs one agent at a time.
    #[must_use]
    pub fn runs(&self) -> Vec<RunStatus> {
        self.latest.iter().cloned().collect()
    }

    /// Where each prompt still waiting for a person is in the log, oldest first.
    #[must_use]
    pub fn waiting(&self) -> Vec<u64> {
        self.waiting.iter().map(|(_, _, seq)| *seq).collect()
    }

    // Another run's prompts can no longer be answered, nor its own once it stops working.
    fn status(&mut self, status: &RunStatus) {
        let working = matches!(
            status.state,
            RunState::Running | RunState::Waiting | RunState::Newer
        );
        self.waiting
            .retain(|(run, _, _)| working && *run == status.run);
        self.latest = Some(status.clone());
    }

    fn said(&mut self, run: &RunId, event: &RunEvent, seq: u64) {
        match event {
            RunEvent::PermissionAsked { prompt, .. } => {
                self.waiting.push((run.clone(), *prompt, seq));
            }
            RunEvent::PermissionWithdrawn { prompt, .. } => self.forget(run, *prompt),
            RunEvent::PermissionAnswered { answer } => self.forget(run, answer.prompt),
            RunEvent::Finished { .. } => self.waiting.retain(|(asked, _, _)| asked != run),
            RunEvent::Started { .. }
            | RunEvent::TextDelta { .. }
            | RunEvent::Text { .. }
            | RunEvent::ToolCall { .. }
            | RunEvent::ToolDone { .. }
            | RunEvent::Newer => {}
        }
    }

    fn forget(&mut self, run: &RunId, prompt: u32) {
        self.waiting
            .retain(|(asked, number, _)| !(asked == run && *number == prompt));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use weacon_protocol::{Outcome, PermissionAnswer};

    use super::*;

    fn run(id: &str) -> RunId {
        RunId::try_from(id.to_owned()).unwrap()
    }

    fn status(id: &str, state: RunState) -> ChatEvent {
        let status = RunStatus {
            run: run(id),
            state,
        };
        ChatEvent::RunStatus { status }
    }

    fn said(id: &str, event: RunEvent) -> ChatEvent {
        let update = RunUpdate {
            run: run(id),
            event,
        };
        ChatEvent::Run { update }
    }

    fn asks(id: &str, prompt: u32) -> ChatEvent {
        let (run, tool, input) = (run(id), "Write".to_owned(), Value::Null);
        said(
            id,
            RunEvent::PermissionAsked {
                run,
                prompt,
                tool,
                input,
            },
        )
    }

    fn answers(id: &str, prompt: u32) -> ChatEvent {
        let answer = PermissionAnswer {
            run: run(id),
            prompt,
            allow: true,
        };
        said(id, RunEvent::PermissionAnswered { answer })
    }

    fn withdraws(id: &str, prompt: u32) -> ChatEvent {
        said(
            id,
            RunEvent::PermissionWithdrawn {
                run: run(id),
                prompt,
            },
        )
    }

    // The log of `events`, numbered from one.
    fn log(events: Vec<ChatEvent>) -> Vec<ChatEntry> {
        (1..)
            .zip(events)
            .map(|(seq, event)| ChatEntry { seq, event })
            .collect()
    }

    #[test]
    fn of_two_prompts_open_only_the_unanswered_one_still_waits() {
        let entries = log(vec![
            status("run-1", RunState::Running),
            asks("run-1", 1),
            asks("run-1", 2),
            status("run-1", RunState::Waiting),
            answers("run-1", 1),
        ]);

        let state = ChatState::fold(&entries);

        assert_eq!(state.waiting(), [3]);
        assert_eq!(
            state.runs(),
            [RunStatus {
                run: run("run-1"),
                state: RunState::Waiting
            }]
        );
    }

    #[test]
    fn an_answer_after_a_withdrawal_or_to_another_run_changes_nothing() {
        let entries = log(vec![
            asks("run-1", 1),
            withdraws("run-1", 1),
            answers("run-1", 1),
            asks("run-1", 2),
            answers("run-9", 2),
        ]);

        assert_eq!(ChatState::fold(&entries).waiting(), [4]);
    }

    #[test]
    fn a_run_that_stops_or_is_replaced_leaves_nothing_waiting() {
        for ends in [
            status("run-1", RunState::Idle),
            status("run-1", RunState::Sleeping),
            status("run-1", RunState::Ended),
            status("run-2", RunState::Starting),
            said(
                "run-1",
                RunEvent::Finished {
                    outcome: Outcome::Unknown,
                },
            ),
        ] {
            let state = ChatState::fold(&log(vec![asks("run-1", 1), ends]));
            assert!(state.waiting().is_empty());
        }
        let still = log(vec![asks("run-1", 1), status("run-1", RunState::Running)]);
        assert_eq!(ChatState::fold(&still).waiting(), [1]);
    }

    #[test]
    fn across_a_restart_only_the_new_runs_prompts_wait() {
        let entries = log(vec![
            asks("run-1", 1),
            // The core restarted: the cut-off run sleeps, and a new one asks with the same number.
            status("run-1", RunState::Sleeping),
            status("run-2", RunState::Starting),
            asks("run-2", 1),
            asks("run-2", 2),
            answers("run-1", 1),
            answers("run-2", 2),
        ]);

        let state = ChatState::fold(&entries);

        assert_eq!(state.waiting(), [4]);
        assert_eq!(
            state.runs(),
            [RunStatus {
                run: run("run-2"),
                state: RunState::Starting
            }]
        );
    }

    #[test]
    fn folding_step_by_step_gives_what_folding_at_once_does() {
        let entries = log(vec![
            ChatEvent::Prompt {
                request: weacon_protocol::RequestId::try_from("r-1".to_owned()).unwrap(),
                text: "Hi".to_owned(),
                repo: None,
            },
            status("run-1", RunState::Running),
            said(
                "run-1",
                RunEvent::Text {
                    text: "Sure.".to_owned(),
                },
            ),
            asks("run-1", 1),
        ]);
        let mut stepped = ChatState::default();

        for entry in &entries {
            stepped.apply(entry);
        }

        assert_eq!(stepped, ChatState::fold(&entries));
        assert_eq!(stepped.waiting(), [4]);
        assert!(ChatState::default().runs().is_empty());
    }
}
