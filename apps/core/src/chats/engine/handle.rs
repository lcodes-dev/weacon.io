//! The clients' way to the engine.

use tokio::sync::{mpsc, oneshot};
use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatCommand, ChatQuery, code};

use crate::chats::feed::Client;

/// The clients' way to the engine.
#[derive(Clone, Debug)]
pub(crate) struct Chats {
    asks: mpsc::Sender<Ask>,
}

pub(super) enum Ask {
    Command(ChatCommand, oneshot::Sender<Result<(), Fault>>),
    Query(
        ChatQuery,
        Client,
        oneshot::Sender<Result<Option<ChatAnswerKind>, Fault>>,
    ),
    /// Whether the disk is full, so no new turn starts.
    DiskFull(bool),
}

fn stopped() -> Fault {
    Fault::new(code!("chat.engine_stopped"), "The daemon is stopping")
}

impl Chats {
    pub(super) const fn new(asks: mpsc::Sender<Ask>) -> Self {
        Self { asks }
    }

    /// Acts on `command`. Its receipts are events in the chat's log; an error means it has no
    /// receipt, and sending it again is safe.
    pub(crate) async fn command(&self, command: ChatCommand) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Command(command, answer);
        self.asks.send(ask).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())?
    }

    /// The answer to `query`, if it has one; a subscription's events go to `client`.
    pub(crate) async fn query(
        &self,
        query: ChatQuery,
        client: &Client,
    ) -> Result<Option<ChatAnswerKind>, Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Query(query, client.clone(), answer);
        self.asks.send(ask).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())?
    }

    /// Tells the engine whether the disk is full: while it is, prompts are queued and no new
    /// turn starts. Never waits; a note the engine had no room for is sent again next time.
    pub(crate) fn disk_full(&self, full: bool) {
        drop(self.asks.try_send(Ask::DiskFull(full)));
    }
}
