//! What differs between the vendors' agents, one row each.

use tokio::process::Command;
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, AgentSession, App, code};

use crate::launch::{Launch, OtherSignIns};
use crate::wire::Wire as RunWire;
use crate::{claude, codex};

pub(crate) struct Vendor {
    /// The variable that points the program at a config folder.
    pub(crate) config_variable: &'static str,
    /// Sign-ins and providers that would win over the workspace's own.
    pub(crate) other_sign_ins: OtherSignIns,
    /// The fixed arguments of the vendor's own sign-in check.
    pub(crate) status: &'static [&'static str],
    /// The fixed arguments of the vendor's own sign-in.
    pub(crate) login: &'static [&'static str],
    /// Whether a check that exits well printed the config folder's own sign-in.
    pub(crate) own_sign_in: fn(&[u8]) -> bool,
    /// Whether a run is pointed at the core's MCP.
    pub(crate) takes_mcp: bool,
    /// A run's command and wire, resuming the session when given.
    pub(crate) drive: fn(&Launch, &App, Option<&AgentSession>) -> (Command, RunWire),
    /// The program on a session, for a person to carry on by hand, where it can be.
    pub(crate) resume: Option<fn(&Launch, &AgentSession) -> Command>,
}

/// # Errors
/// `agent.unknown` for an agent this build doesn't know.
pub(crate) fn vendor(agent: AgentKind) -> Result<&'static Vendor, Fault> {
    match agent {
        AgentKind::Claude => Ok(&claude::VENDOR),
        AgentKind::Codex => Ok(&codex::VENDOR),
        AgentKind::Newer => Err(unknown()),
    }
}

pub(crate) fn unknown() -> Fault {
    Fault::new(code!("agent.unknown"), "This build doesn't know that agent")
}
