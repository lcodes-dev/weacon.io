//! Which part of the daemon answers each message a client sends.

use weacon_protocol::{Envelope, FrameError, Message};

use super::worktrees::{self, Late};
use super::{Shared, command_chat, keep, query_chat};
use crate::chats::feed::Client;
use crate::session::{Reply, Session};

// The reply to one message from a client; only a greeted one is served.
pub(super) async fn route(
    shared: &Shared,
    session: &mut Session,
    received: Result<Envelope, FrameError>,
    client: &Client,
    late: &Late,
) -> Reply {
    let Envelope { id, message } = match received {
        Ok(envelope) if session.greeted() => envelope,
        received => return session.answer(received, &shared.app),
    };
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every other message is the session's to answer"
    )]
    match message {
        Message::Pick(pick) => keep(shared, id, pick).await,
        Message::ChatCommand(command) => command_chat(shared, id, command).await,
        Message::ChatQuery(query) => query_chat(shared, id, query, client).await,
        Message::DiffQuery(query) => worktrees::diff(shared, id, query, late),
        Message::WorktreeQuery(query) => worktrees::query(shared, id, query, late),
        Message::WorktreeCommand(command) => worktrees::command(shared, id, command, late),
        message => session.answer(Ok(Envelope { id, message }), &shared.app),
    }
}
