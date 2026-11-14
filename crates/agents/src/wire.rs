//! What every agent's driver says to the run, and the run to each driver.

use std::path::PathBuf;

use serde_json::Value;
use weacon_protocol::{AgentSession, RunEvent};

use crate::{claude, codex};

/// Sent back for a request the driver doesn't handle.
pub(crate) const UNSUPPORTED: &str = "Weacon doesn't handle this request.";

/// Each agent's own way of saying things.
pub(crate) enum Wire {
    Claude(claude::Wire),
    Codex(codex::Wire),
}

/// What an agent's wire made of a line or a request, for the run to act on.
pub(crate) enum Effect {
    Send(Value),
    Started(AgentSession),
    Event(RunEvent),
    Asked(Asked),
    Withdrawn { request: Value },
    Finished { ok: bool },
}

/// An agent's request for permission to use `tool` with `input`.
pub(crate) struct Asked {
    /// The agent's own id for the request, sent back with the answer.
    pub(crate) request: Value,
    pub(crate) tool: String,
    pub(crate) input: Value,
}

impl Wire {
    pub(crate) fn opening(&mut self) -> Vec<Effect> {
        match self {
            Self::Claude(_) => Vec::new(),
            Self::Codex(wire) => wire.opening(),
        }
    }

    pub(crate) fn prompt(&mut self, text: &str) -> Vec<Effect> {
        match self {
            Self::Claude(_) => claude::prompt(text),
            Self::Codex(wire) => wire.prompt(text),
        }
    }

    pub(crate) fn answer(&self, asked: &Asked, allow: bool) -> Vec<Effect> {
        match self {
            Self::Claude(_) => claude::answer(&asked.request, allow, &asked.input),
            Self::Codex(_) => codex::answer(&asked.request, allow),
        }
    }

    pub(crate) fn interrupt(&mut self) -> Vec<Effect> {
        match self {
            Self::Claude(wire) => wire.interrupt(),
            Self::Codex(wire) => wire.interrupt(),
        }
    }

    // Codex's file changes always ask a person.
    pub(crate) fn changed_file(&self, asked: &Asked) -> Option<PathBuf> {
        match self {
            Self::Claude(_) => claude::changed_file(&asked.tool, &asked.input),
            Self::Codex(_) => None,
        }
    }

    pub(crate) fn read(&mut self, line: &[u8]) -> Vec<Effect> {
        match self {
            Self::Claude(_) => claude::read(line),
            Self::Codex(wire) => wire.read(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_the_agent_reads_keeps_its_size() {
        assert_eq!(UNSUPPORTED.len(), 35);
    }
}
