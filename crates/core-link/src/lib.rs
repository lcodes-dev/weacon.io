use serde::{Deserialize, Serialize};

pub const CONNECT: &str = "/api/machines/connect";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Claude, Agent::Codex];

    pub fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    pub fn named(name: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|agent| agent.program() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ready {
    Missing,
    SignedOut,
    SignedIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Found {
    pub agent: Agent,
    pub ready: Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Deed {
    Read,
    Edit,
    Command,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Changed {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Said {
        text: String,
    },
    Did {
        deed: Deed,
        what: String,
    },
    Done {
        session: String,
        changed: Vec<Changed>,
        #[serde(default)]
        diff: String,
    },
    Failed {
        why: String,
    },
    Wants {
        host: String,
    },
}

pub fn host(name: &str) -> bool {
    let label = |label: &str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    name.len() <= 253
        && name.contains('.')
        && name.split('.').all(label)
        && name.chars().any(|c| c.is_ascii_lowercase())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Turn {
    pub chat: String,
    pub agent: Agent,
    pub session: Option<String>,
    pub prompt: String,
    pub repository: Repository,
    pub heard: u64,
    #[serde(default)]
    pub hosts: Vec<String>,
    pub tools: Tools,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tools {
    pub url: String,
    pub token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Repository {
    pub url: String,
    pub branch: String,
    pub token: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromCore {
    Agents {
        agents: Vec<Found>,
    },
    Answer {
        id: String,
        answer: Result<String, String>,
    },
    Event {
        chat: String,
        number: u64,
        event: Event,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        agent: Agent,
        data: String,
    },
    #[serde(rename = "terminal.close")]
    TerminalClose {
        agent: Agent,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToCore {
    Ask {
        id: String,
        agent: Agent,
        model: String,
        system: String,
        prompt: String,
    },
    Turn(Turn),
    Stop {
        chat: String,
    },
    Push {
        id: String,
        chat: String,
        repository: Repository,
        to: String,
        message: String,
        name: String,
        mail: String,
    },
    Forget {
        id: String,
        chat: String,
    },
    Replaced,
    Taken {
        id: String,
    },
    #[serde(rename = "terminal.open")]
    TerminalOpen {
        agent: Agent,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        agent: Agent,
        data: String,
    },
    #[serde(rename = "terminal.size")]
    TerminalSize {
        agent: Agent,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.close")]
    TerminalClose {
        agent: Agent,
    },
}
