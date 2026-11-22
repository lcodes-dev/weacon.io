//! What the server and a core say to each other over the core's WebSocket: JSON, one message a frame.

use serde::{Deserialize, Serialize};

/// The path the core's WebSocket is at, under the server's address.
pub const CONNECT: &str = "/api/machines/connect";

/// A coding agent's own program on a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Claude, Agent::Codex];

    /// The program's name, which is also how the agent is written in an address.
    pub fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn named(name: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|agent| agent.program() == name)
    }
}

/// Whether an agent can be used on a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ready {
    /// Its program isn't on the machine.
    Missing,
    /// Its program is there, and nobody is signed in to it.
    SignedOut,
    SignedIn,
}

/// An agent as a machine finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Found {
    pub agent: Agent,
    pub ready: Ready,
}

/// What an agent may be seen doing in a chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Deed {
    Read,
    Edit,
    Command,
    /// Anything else, named by the agent's own word for it.
    Other,
}

/// A file a chat's folder holds changed, against the commit it started from.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Changed {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// What happens in a chat's turn on a machine, the same for every agent.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// The agent's words.
    Said {
        text: String,
    },
    /// Something the agent did: `what` is the file, the command, or the tool's name.
    Did {
        deed: Deed,
        what: String,
    },
    /// The turn ended. `session` is how the agent's program knows the conversation next time.
    Done {
        session: String,
        changed: Vec<Changed>,
    },
    Failed {
        why: String,
    },
}

/// Where a chat's folder gets its files the first time.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Repository {
    /// The address to fetch from, over https.
    pub url: String,
    pub branch: String,
    /// Good for this repository alone, for an hour. Used to fetch and kept nowhere.
    pub token: String,
}

/// What a core tells the server.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromCore {
    /// The agents on the machine, sent on connecting and whenever it may have changed.
    Agents { agents: Vec<Found> },
    /// What an agent answered to an `Ask`, or why it didn't. Sent again after a dropped
    /// connection until the server says `Taken`.
    Answer {
        id: String,
        answer: Result<String, String>,
    },
    /// The `number`th event of a chat on this machine, counted from 1.
    Event {
        chat: String,
        number: u64,
        event: Event,
    },
    /// What a sign-in's terminal shows, as base64.
    #[serde(rename = "terminal.data")]
    TerminalData { agent: Agent, data: String },
    /// The sign-in's command ended, or couldn't start.
    #[serde(rename = "terminal.close")]
    TerminalClose { agent: Agent },
}

/// What the server tells a core.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToCore {
    /// Have an agent answer one prompt, with no tools and none of the person's own settings.
    Ask {
        id: String,
        agent: Agent,
        /// Empty for the agent's cheap default.
        model: String,
        system: String,
        prompt: String,
    },
    /// Run a chat's turn in its folder, or go on telling of the one that is running. `heard`
    /// is how many of the chat's events the server has, so the core sends what came after.
    Turn {
        chat: String,
        agent: Agent,
        /// The conversation to go on with, if the chat has had a turn.
        session: Option<String>,
        prompt: String,
        repository: Repository,
        heard: u64,
    },
    /// End a chat's turn.
    Stop { chat: String },
    /// Another core has connected as this machine, so this one must stop and not dial again.
    Replaced,
    /// The server has the answer to this `Ask`, so the core may forget it.
    Taken { id: String },
    /// Start the agent's own sign-in in a terminal of this size. The core picks the command.
    #[serde(rename = "terminal.open")]
    TerminalOpen { agent: Agent, cols: u16, rows: u16 },
    /// What the person typed, as base64.
    #[serde(rename = "terminal.data")]
    TerminalData { agent: Agent, data: String },
    #[serde(rename = "terminal.size")]
    TerminalSize { agent: Agent, cols: u16, rows: u16 },
    /// The person closed the terminal.
    #[serde(rename = "terminal.close")]
    TerminalClose { agent: Agent },
}
