//! What a chat's log says now: where its latest run stands and which permission prompts wait
//! for a person.

use weacon_protocol::{ChatEntry, ChatEvent, RunEvent, RunId, RunState, RunStatus, RunUpdate};

/// What a chat's log folds into, one entry at a time, in log order from its first entry.
/// Folding a window that starts later gives what only that window holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatRuns {
    /// The chat's latest run, as its last status says; a chat runs one agent at a time.
    pub runs: Vec<RunStatus>,
    /// The prompts still waiting for a person, oldest first.
    pub waiting: Vec<Waiting>,
}

/// A permission prompt waiting for a person: the run that asked, its number, and the place of
/// the entry that asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub run: RunId,
    pub prompt: u32,
    pub seq: u64,
}

impl ChatRuns {
    /// What `entries` fold into.
    #[must_use]
    pub fn fold(entries: &[ChatEntry]) -> Self {
        let mut runs = Self::default();
        for entry in entries {
            runs.apply(entry);
        }
        runs
    }

    /// Takes in the log's next entry.
    pub fn apply(&mut self, entry: &ChatEntry) {
        match &entry.event {
            ChatEvent::RunStatus { status } => self.status(status),
            ChatEvent::Run {
                update: RunUpdate { run, event },
            } => self.said(run, event, entry.seq),
            ChatEvent::Prompt { .. }
            | ChatEvent::Receipt { .. }
            | ChatEvent::OutsidePrompt { .. }
            | ChatEvent::Newer => {}
        }
    }

    /// Whether `run`'s prompt `prompt` waits for a person.
    #[must_use]
    pub fn waits(&self, run: &RunId, prompt: u32) -> bool {
        self.waiting
            .iter()
            .any(|waits| waits.run == *run && waits.prompt == prompt)
    }

    /// Where each waiting prompt was asked in the log, as a snapshot sends it.
    #[must_use]
    pub fn waiting_seqs(&self) -> Vec<u64> {
        self.waiting.iter().map(|waits| waits.seq).collect()
    }

    // Another run's prompts can no longer be answered, nor its own once it stops working.
    fn status(&mut self, status: &RunStatus) {
        let working = matches!(
            status.state,
            RunState::Running | RunState::Waiting | RunState::Newer
        );
        self.waiting
            .retain(|waits| working && waits.run == status.run);
        self.runs = vec![status.clone()];
    }

    fn said(&mut self, run: &RunId, event: &RunEvent, seq: u64) {
        match event {
            RunEvent::PermissionAsked { prompt, .. } => self.waiting.push(Waiting {
                run: run.clone(),
                prompt: *prompt,
                seq,
            }),
            RunEvent::PermissionWithdrawn { prompt, .. } => self.forget(run, *prompt),
            RunEvent::PermissionAnswered { answer } => self.forget(run, answer.prompt),
            RunEvent::Finished { .. } => self.waiting.retain(|waits| waits.run != *run),
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
            .retain(|waits| !(waits.run == *run && waits.prompt == prompt));
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
            agent: None,
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
            asks("run-1", 2),
            status("run-1", RunState::Waiting),
            answers("run-1", 1),
        ]);

        let state = ChatRuns::fold(&entries);

        assert_eq!(state.waiting_seqs(), [5]);
        assert!(state.waits(&run("run-1"), 2));
        assert!(!state.waits(&run("run-1"), 1) && !state.waits(&run("run-2"), 2));
        assert_eq!(
            state.runs,
            [RunStatus {
                run: run("run-1"),
                state: RunState::Waiting,
                agent: None
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

        assert_eq!(ChatRuns::fold(&entries).waiting_seqs(), [4]);
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
            let state = ChatRuns::fold(&log(vec![asks("run-1", 1), ends]));
            assert!(state.waiting_seqs().is_empty());
        }
        let still = log(vec![asks("run-1", 1), status("run-1", RunState::Running)]);
        assert_eq!(ChatRuns::fold(&still).waiting_seqs(), [1]);
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

        let state = ChatRuns::fold(&entries);

        assert_eq!(state.waiting_seqs(), [4]);
        assert_eq!(
            state.runs,
            [RunStatus {
                run: run("run-2"),
                state: RunState::Starting,
                agent: None
            }]
        );
    }
}
