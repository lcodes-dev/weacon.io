use std::collections::HashMap;

use weacon_protocol::{Outcome, RunEvent};

/// One row of a run's transcript. A text row's text lives in the view that renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Text,
    Tool { tool: String, state: ToolState },
    Finished(Outcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolState {
    Running,
    Done,
    Failed,
}

/// What an event changed, for the view to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change<'a> {
    /// A row was added at the end.
    Added(usize),
    /// Text was added to the text row being written.
    Appended(usize, &'a str),
    /// The text row was written in full, replacing what streamed into it.
    Replaced(usize, &'a str),
    /// A tool row changed state.
    Updated(usize),
}

/// The rows of one run, built up from its events in order.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    rows: Vec<Row>,
    writing: Option<usize>,
    calls: HashMap<String, usize>,
}

impl Transcript {
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Applies `event`, giving what changed in order: a new row comes before its text.
    pub(crate) fn apply<'a>(&mut self, event: &'a RunEvent) -> Vec<Change<'a>> {
        match event {
            RunEvent::TextDelta { text } => self.write(text, Change::Appended, true),
            RunEvent::Text { text } => self.write(text, Change::Replaced, false),
            RunEvent::ToolCall { call, tool, .. } => {
                let index = self.push(Row::Tool {
                    tool: tool.clone(),
                    state: ToolState::Running,
                });
                self.calls.insert(call.clone(), index);
                vec![Change::Added(index)]
            }
            RunEvent::ToolDone { call, failed } => self.finish_tool(call, *failed),
            RunEvent::Finished { outcome } => {
                vec![Change::Added(self.push(Row::Finished(outcome.clone())))]
            }
            RunEvent::Started { .. }
            | RunEvent::PermissionAsked { .. }
            | RunEvent::PermissionWithdrawn { .. }
            | RunEvent::Newer => Vec::new(),
        }
    }

    fn write<'a>(
        &mut self,
        text: &'a str,
        change: fn(usize, &'a str) -> Change<'a>,
        more: bool,
    ) -> Vec<Change<'a>> {
        let mut changes = Vec::new();
        let index = self.writing.unwrap_or_else(|| {
            let index = self.push(Row::Text);
            changes.push(Change::Added(index));
            index
        });
        self.writing = more.then_some(index);
        changes.push(change(index, text));
        changes
    }

    fn finish_tool(&mut self, call: &str, failed: bool) -> Vec<Change<'static>> {
        let Some(&index) = self.calls.get(call) else {
            return Vec::new();
        };
        if let Some(Row::Tool { state, .. }) = self.rows.get_mut(index) {
            *state = if failed {
                ToolState::Failed
            } else {
                ToolState::Done
            };
        }
        self.calls.remove(call);
        vec![Change::Updated(index)]
    }

    // Any other row ends the text being written.
    fn push(&mut self, row: Row) -> usize {
        if row != Row::Text {
            self.writing = None;
        }
        self.rows.push(row);
        self.rows.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn delta(text: &str) -> RunEvent {
        RunEvent::TextDelta {
            text: text.to_owned(),
        }
    }

    fn text(text: &str) -> RunEvent {
        RunEvent::Text {
            text: text.to_owned(),
        }
    }

    fn call(call: &str) -> RunEvent {
        RunEvent::ToolCall {
            call: call.to_owned(),
            tool: "Read".to_owned(),
            input: json!({}),
        }
    }

    fn done(call: &str, failed: bool) -> RunEvent {
        RunEvent::ToolDone {
            call: call.to_owned(),
            failed,
        }
    }

    #[test]
    fn deltas_stream_into_one_row_that_the_whole_text_then_replaces() {
        let mut transcript = Transcript::default();
        let events = [delta("He"), delta("llo"), text("Hello."), text("Next.")];
        let changes: Vec<_> = events.iter().map(|event| transcript.apply(event)).collect();

        assert_eq!(
            changes,
            [
                vec![Change::Added(0), Change::Appended(0, "He")],
                vec![Change::Appended(0, "llo")],
                vec![Change::Replaced(0, "Hello.")],
                vec![Change::Added(1), Change::Replaced(1, "Next.")],
            ]
        );
        assert_eq!(transcript.rows(), [Row::Text, Row::Text]);
    }

    #[test]
    fn tools_get_rows_that_change_when_they_finish_and_end_the_text() {
        let mut transcript = Transcript::default();
        for event in [delta("Reading"), call("c-1"), call("c-2")] {
            transcript.apply(&event);
        }
        let ok = done("c-1", false);
        let failed = done("c-2", true);
        let again = done("c-1", true);
        let after = delta("Done");

        assert_eq!(transcript.apply(&ok), [Change::Updated(1)]);
        assert_eq!(transcript.apply(&failed), [Change::Updated(2)]);
        assert_eq!(transcript.apply(&again), []);
        assert_eq!(
            transcript.apply(&after),
            [Change::Added(3), Change::Appended(3, "Done")]
        );
        assert_eq!(
            transcript.rows().get(1..3).unwrap(),
            [
                Row::Tool {
                    tool: "Read".to_owned(),
                    state: ToolState::Done
                },
                Row::Tool {
                    tool: "Read".to_owned(),
                    state: ToolState::Failed
                },
            ]
        );
    }

    #[test]
    fn a_finished_turn_ends_the_text_and_other_events_change_nothing() {
        let mut transcript = Transcript::default();
        let finished = RunEvent::Finished {
            outcome: Outcome::Completed,
        };
        transcript.apply(&delta("Bye"));

        assert_eq!(transcript.apply(&finished), [Change::Added(1)]);
        assert_eq!(transcript.apply(&RunEvent::Newer), []);
        assert_eq!(transcript.apply(&done("c-9", false)), []);
        assert_eq!(
            transcript.apply(&delta("Hi")),
            [Change::Added(2), Change::Appended(2, "Hi")]
        );
        assert_eq!(
            transcript.rows().get(1),
            Some(&Row::Finished(Outcome::Completed))
        );
    }
}
