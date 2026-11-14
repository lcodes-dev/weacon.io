//! Sessions a person ran outside Weacon, read back from the files their agent keeps.

use std::path::PathBuf;

use weacon_protocol::{AgentKind, RunEvent};

use crate::{claude, codex};

/// One step of a session, in the order the file holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// What the person typed.
    Prompt(String),
    Event(RunEvent),
}

/// What a session file says, as far as this build reads it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    /// The folder the session ran in, when the file names it.
    pub cwd: Option<PathBuf>,
    pub steps: Vec<Step>,
    /// Complete lines that didn't parse.
    pub skipped: usize,
    /// Where the complete lines end: a later read of the same file starts there.
    pub end: u64,
}

impl Transcript {
    /// The complete lines of `agent`'s session `file`; a last line without its newline is still
    /// being written, and is left out of [`Transcript::end`]. `None` for an agent this build
    /// doesn't know.
    #[must_use]
    pub fn read(agent: AgentKind, file: &[u8]) -> Option<Self> {
        let line = match agent {
            AgentKind::Claude => claude::transcript::read_line,
            AgentKind::Codex => codex::transcript::read_line,
            AgentKind::Newer => return None,
        };
        let mut transcript = Self::default();
        let complete = file
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|chunk| chunk.ends_with(b"\n"));
        for chunk in complete {
            let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            transcript.end = transcript.end.saturating_add(len);
            if !chunk.trim_ascii().is_empty() && line(chunk, &mut transcript).is_err() {
                transcript.skipped += 1;
            }
        }
        Some(transcript)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_lines_are_read_and_the_rest_are_counted() {
        let file = b"not json\n\n{\"type\":\"summary\"}\n{\"type\":\"user\",\"message\":";

        let transcript = Transcript::read(AgentKind::Claude, file).unwrap();

        assert_eq!(
            transcript,
            Transcript {
                skipped: 1,
                end: 29,
                ..Transcript::default()
            }
        );
    }

    #[test]
    fn an_agent_this_build_doesnt_know_has_no_transcript() {
        assert_eq!(Transcript::read(AgentKind::Newer, b"{}\n"), None);
    }
}
