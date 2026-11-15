//! The fake chat's events, built and read.

use serde_json::Value;
use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, ChatAction, ChatCommand, ChatEvent, ChatQuery, ChatQueryKind,
    MachineName, Message, RunEvent, RunId, RunUpdate,
};

/// Whether a chat query or command holds a part this build doesn't know, such as a prompt to
/// an agent it can't name.
pub(super) const fn unknown(message: &Message) -> bool {
    matches!(
        message,
        Message::ChatQuery(ChatQuery {
            query: ChatQueryKind::Newer,
            ..
        }) | Message::ChatCommand(ChatCommand {
            action: ChatAction::Newer
                | ChatAction::Prompt {
                    agent: Some(AgentKind::Newer),
                    ..
                },
            ..
        })
    )
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

// The run's first permission prompt, to use Bash.
pub(super) fn asks_bash(run: &RunId) -> ChatEvent {
    ChatEvent::Run {
        update: RunUpdate {
            run: run.clone(),
            event: RunEvent::PermissionAsked {
                run: run.clone(),
                prompt: 1,
                tool: "Bash".to_owned(),
                input: Value::Null,
            },
        },
    }
}
