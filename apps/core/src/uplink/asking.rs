//! Who asks the machine to act, as each message from the server says.

use weacon_protocol::{AccountId, Asker, Message};

use crate::proposals;

/// Who asks the machine to act, in a message from the server.
pub(super) enum Asking<'a> {
    /// The account the server stamped as the asker, if it did.
    By(Option<&'a AccountId>),
    /// Something the server says of its own, such as an error.
    NotAnAsk,
    /// A kind with no arm in [`asking`], refused until one says where its asker is.
    Unknown,
}

pub(super) fn asking(message: &Message) -> Asking<'_> {
    let asker = if let Message::ChatCommand(command) = message {
        &command.by
    } else if let Message::MachineSettingsQuery(query) = message {
        &query.by
    } else if let Message::MachineSettingsCommand(command) = message {
        &command.by
    } else if let Message::TerminalCommand(command) = message {
        &command.by
    } else if let Message::DiffQuery(query) = message {
        &query.by
    } else if let Message::WorktreeQuery(query) = message {
        &query.by
    } else if let Message::WorktreeCommand(command) = message {
        &command.by
    } else if let Message::SymbolsQuery(query) = message {
        &query.by
    } else if let Message::SessionsQuery(query) = message {
        return Asking::By(query.by.as_ref());
    } else if let Message::Error(_) | Message::ChatQuery(_) = message {
        // A chat query is the server following a chat it stores.
        return Asking::NotAnAsk;
    } else if proposals::answers(message) {
        return Asking::NotAnAsk;
    } else {
        return Asking::Unknown;
    };
    Asking::By(asker.as_ref().map(|asker: &Asker| &asker.account))
}
