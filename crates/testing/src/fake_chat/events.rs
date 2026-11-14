//! The fake chat's events, built and read.

use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, ChatEntry, ChatEvent, MachineName, RunEvent, RunId, RunState,
    RunUpdate,
};

// The run and prompt of the permission prompt `entry` is, if it is one.
pub(super) fn asked(entry: &ChatEntry) -> Option<(&RunId, u32)> {
    let ChatEvent::Run { update } = &entry.event else {
        return None;
    };
    let RunEvent::PermissionAsked { prompt, .. } = update.event else {
        return None;
    };
    Some((&update.run, prompt))
}

// Whether `entry` ends the wait for the prompt `prompt` of `run`.
pub(super) fn closes(entry: &ChatEntry, run: &RunId, prompt: u32) -> bool {
    if let ChatEvent::RunStatus { status } = &entry.event {
        return status.run == *run && status.state != RunState::Waiting;
    }
    let ChatEvent::Run { update } = &entry.event else {
        return false;
    };
    update.run == *run
        && (matches!(update.event, RunEvent::Finished { .. })
            || matches!(update.event, RunEvent::PermissionWithdrawn { prompt: withdrawn, .. } if withdrawn == prompt)
            || matches!(&update.event, RunEvent::PermissionAnswered { answer } if answer.prompt == prompt))
}

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
