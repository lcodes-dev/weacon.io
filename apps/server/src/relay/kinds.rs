//! What each kind the relay carries asks, needs and answers.

use weacon_protocol::{
    Asker, ChatId, Code, MachineId, Message, RequestId, WorkspaceId, WorktreeAction,
};

use super::{DIFF, SETTINGS, TERMINAL, WORKTREE};

/// The workspace, request and subject an ask or its answer names.
#[derive(PartialEq, Eq)]
pub(super) struct Asked {
    pub(super) workspace: WorkspaceId,
    pub(super) request: RequestId,
    pub(super) about: About,
}

/// What an ask is about: a machine it names, or a chat, whose machine the server knows.
#[derive(PartialEq, Eq)]
pub(super) enum About {
    Machine(MachineId),
    Chat(ChatId),
}

/// What a client offers for the relay to carry `message`, which the machine's core offers too;
/// `None` for a kind the relay doesn't carry.
pub(crate) const fn needs(message: &Message) -> Option<Code> {
    if matches!(
        message,
        Message::MachineSettingsQuery(_) | Message::MachineSettingsCommand(_)
    ) {
        Some(SETTINGS)
    } else if matches!(message, Message::DiffQuery(_)) {
        Some(DIFF)
    } else if matches!(
        message,
        Message::WorktreeQuery(_) | Message::WorktreeCommand(_)
    ) {
        Some(WORKTREE)
    } else {
        None
    }
}

/// What the machine's core must offer to be sent `message`.
pub(super) fn machine_needs(message: &Message) -> Option<Code> {
    if let Message::WorktreeCommand(command) = message
        && command.action == WorktreeAction::OpenTerminal
    {
        return Some(TERMINAL);
    }
    needs(message)
}

/// What a message the relay carries asks, with `by` stamped on it whatever the client
/// put there; `None` for a kind the relay doesn't carry.
pub(super) fn stamped(mut message: Message, by: &Asker) -> Option<(Asked, Message)> {
    let by = Some(by.clone());
    let asked = |workspace: &WorkspaceId, request: &RequestId, about| Asked {
        workspace: workspace.clone(),
        request: request.clone(),
        about,
    };
    let asked = if let Message::MachineSettingsQuery(query) = &mut message {
        query.by = by;
        let about = About::Machine(query.machine.clone());
        asked(&query.workspace, &query.request, about)
    } else if let Message::MachineSettingsCommand(command) = &mut message {
        command.by = by;
        let about = About::Machine(command.machine.clone());
        asked(&command.workspace, &command.request, about)
    } else if let Message::DiffQuery(query) = &mut message {
        query.by = by;
        asked(
            &query.workspace,
            &query.request,
            About::Chat(query.chat.clone()),
        )
    } else if let Message::WorktreeQuery(query) = &mut message {
        query.by = by;
        asked(
            &query.workspace,
            &query.request,
            About::Chat(query.chat.clone()),
        )
    } else if let Message::WorktreeCommand(command) = &mut message {
        command.by = by;
        asked(
            &command.workspace,
            &command.request,
            About::Chat(command.chat.clone()),
        )
    } else {
        return None;
    };
    Some((asked, message))
}

/// Whether `message`, from a machine, is an answer or an error the relay may take back.
pub(crate) fn takes(message: &Message) -> bool {
    matches!(message, Message::Error(_)) || answering(message).is_some()
}

/// What an answer the relay carries back names.
pub(super) fn answering(message: &Message) -> Option<Asked> {
    let (workspace, request, about) = if let Message::MachineSettingsAnswer(answer) = message {
        let about = About::Machine(answer.machine.clone());
        (&answer.workspace, &answer.request, about)
    } else if let Message::DiffAnswer(answer) = message {
        (
            &answer.workspace,
            &answer.request,
            About::Chat(answer.chat.clone()),
        )
    } else if let Message::WorktreeAnswer(answer) = message {
        (
            &answer.workspace,
            &answer.request,
            About::Chat(answer.chat.clone()),
        )
    } else {
        return None;
    };
    Some(Asked {
        workspace: workspace.clone(),
        request: request.clone(),
        about,
    })
}
