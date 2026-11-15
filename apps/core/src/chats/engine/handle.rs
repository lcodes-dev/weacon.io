//! The clients' way to the engine.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};
use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatCommand, ChatQuery, code};

use crate::chats::feed::Client;

/// The clients' way to the engine.
#[derive(Clone, Debug)]
pub(crate) struct Chats {
    asks: mpsc::Sender<Ask>,
    /// Whether the disk is full; it holds the latest, so no note is ever lost.
    disk_full: Arc<watch::Sender<bool>>,
}

pub(super) enum Ask {
    Command(ChatCommand, oneshot::Sender<Result<(), Fault>>),
    Busy(oneshot::Sender<bool>),
    Query(
        ChatQuery,
        Client,
        oneshot::Sender<Result<Option<ChatAnswerKind>, Fault>>,
    ),
}

fn stopped() -> Fault {
    Fault::new(code!("chat.engine_stopped"), "The daemon is stopping")
}

/// A way to the engine that lets it stop once every [`Chats`] is gone.
#[derive(Clone, Debug)]
pub(crate) struct WeakChats(mpsc::WeakSender<Ask>);

impl WeakChats {
    /// Whether an agent is on a turn or waits on a person, or a session or a turn's end the
    /// journal couldn't save yet is only in memory, which a restart would cut off or lose.
    pub(crate) async fn busy(&self) -> Result<bool, Fault> {
        let asks = self.0.upgrade().ok_or_else(stopped)?;
        let (answer, answered) = oneshot::channel();
        asks.send(Ask::Busy(answer)).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())
    }
}

impl Chats {
    pub(super) fn new(asks: mpsc::Sender<Ask>, disk_full: watch::Sender<bool>) -> Self {
        let disk_full = Arc::new(disk_full);
        Self { asks, disk_full }
    }

    /// Acts on `command`. Its receipts are events in the chat's log; an error means it has no
    /// receipt, and sending it again is safe.
    pub(crate) async fn command(&self, command: ChatCommand) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Command(command, answer);
        self.asks.send(ask).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())?
    }

    /// A way to ask whether the engine is busy that doesn't keep it running.
    pub(crate) fn downgrade(&self) -> WeakChats {
        WeakChats(self.asks.downgrade())
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
    /// turn starts. Never waits, and only a change wakes the engine.
    pub(crate) fn disk_full(&self, full: bool) {
        self.disk_full
            .send_if_modified(|was| std::mem::replace(was, full) != full);
    }
}
