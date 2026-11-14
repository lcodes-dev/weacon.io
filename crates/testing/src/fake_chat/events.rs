//! The fake chat's events, built and read.

use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, ChatEvent, MachineName, RunEvent, RunId, RunUpdate,
};

pub(super) fn run(id: &str) -> Result<RunId, String> {
    RunId::try_from(id.to_owned())
}

pub(super) fn agent(kind: AgentKind, owner: AgentOwner, machine: &str) -> Result<AgentRef, String> {
    Ok(AgentRef {
        kind,
        owner,
        machine: Some(MachineName::try_from(machine.to_owned())?),
    })
}

pub(super) fn said(run: &RunId, text: &str) -> ChatEvent {
    ChatEvent::Run {
        update: RunUpdate {
            run: run.clone(),
            event: RunEvent::Text {
                text: text.to_owned(),
            },
        },
    }
}
