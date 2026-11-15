//! What each kind the relay carries asks, needs and answers.

use weacon_protocol::{
    Asker, ChatId, Code, MachineId, Message, RequestId, WorkspaceId, WorktreeAction,
};

use super::{DIFF, SESSIONS, SETTINGS, SYMBOLS, TERMINAL, WORKTREE};

/// The workspace, request and subject an ask or its answer names.
#[derive(PartialEq, Eq)]
pub(super) struct Asked {
    /// None for a person's own sessions, which no workspace holds.
    pub(super) workspace: Option<WorkspaceId>,
    pub(super) request: RequestId,
    pub(super) about: About,
}

/// What an ask is about: a machine it names, its owner's own sessions or code on a machine, or a
/// chat's diff or worktree, whose machine the server knows. Its answer must be about the same.
#[derive(PartialEq, Eq)]
pub(super) enum About {
    Machine(MachineId),
    Sessions(MachineId),
    Symbols(MachineId),
    Diff(ChatId),
    Worktree(ChatId),
}

impl Asked {
    fn sessions(machine: &MachineId, request: &RequestId) -> Self {
        Self {
            workspace: None,
            request: request.clone(),
            about: About::Sessions(machine.clone()),
        }
    }

    fn new(workspace: &WorkspaceId, request: &RequestId, about: About) -> Self {
        Self {
            workspace: Some(workspace.clone()),
            request: request.clone(),
            about,
        }
    }
}

/// What a client offers for the relay to carry `message`, which the machine's core offers too;
/// `None` for a kind the relay doesn't carry.
pub(crate) const fn needs(message: &Message) -> Option<Code> {
    if matches!(
        message,
        Message::MachineSettingsQuery(_) | Message::MachineSettingsCommand(_)
    ) {
        Some(SETTINGS)
    } else if matches!(message, Message::SessionsQuery(_)) {
        Some(SESSIONS)
    } else if matches!(message, Message::DiffQuery(_)) {
        Some(DIFF)
    } else if matches!(message, Message::SymbolsQuery(_)) {
        Some(SYMBOLS)
    } else if matches!(
        message,
        Message::WorktreeQuery(_) | Message::WorktreeCommand(_)
    ) {
        Some(WORKTREE)
    } else {
        None
    }
}

/// What the machine's core must offer to be sent `message`, which may be more than [`needs`].
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
    let asked = if let Message::MachineSettingsQuery(query) = &mut message {
        query.by = by;
        let about = About::Machine(query.machine.clone());
        Asked::new(&query.workspace, &query.request, about)
    } else if let Message::MachineSettingsCommand(command) = &mut message {
        command.by = by;
        let about = About::Machine(command.machine.clone());
        Asked::new(&command.workspace, &command.request, about)
    } else if let Message::SessionsQuery(query) = &mut message {
        // No workspace, so no member: only the account.
        query.by = by.map(|asker| asker.account);
        Asked::sessions(&query.machine, &query.request)
    } else if let Message::DiffQuery(query) = &mut message {
        query.by = by;
        let about = About::Diff(query.chat.clone());
        Asked::new(&query.workspace, &query.request, about)
    } else if let Message::SymbolsQuery(query) = &mut message {
        query.by = by;
        let about = About::Symbols(query.machine.clone());
        Asked::new(&query.workspace, &query.request, about)
    } else if let Message::WorktreeQuery(query) = &mut message {
        query.by = by;
        let about = About::Worktree(query.chat.clone());
        Asked::new(&query.workspace, &query.request, about)
    } else if let Message::WorktreeCommand(command) = &mut message {
        command.by = by;
        let about = About::Worktree(command.chat.clone());
        Asked::new(&command.workspace, &command.request, about)
    } else {
        return None;
    };
    Some((asked, message))
}

/// What an answer the relay carries back names.
pub(super) fn answering(message: &Message) -> Option<Asked> {
    if let Message::MachineSettingsAnswer(answer) = message {
        let about = About::Machine(answer.machine.clone());
        Some(Asked::new(&answer.workspace, &answer.request, about))
    } else if let Message::SessionsAnswer(answer) = message {
        Some(Asked::sessions(&answer.machine, &answer.request))
    } else if let Message::DiffAnswer(answer) = message {
        let about = About::Diff(answer.chat.clone());
        Some(Asked::new(&answer.workspace, &answer.request, about))
    } else if let Message::SymbolsAnswer(answer) = message {
        let about = About::Symbols(answer.machine.clone());
        Some(Asked::new(&answer.workspace, &answer.request, about))
    } else if let Message::WorktreeAnswer(answer) = message {
        let about = About::Worktree(answer.chat.clone());
        Some(Asked::new(&answer.workspace, &answer.request, about))
    } else {
        None
    }
}
