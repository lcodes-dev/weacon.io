//! The clients' way to the engine, and how the engine answers them.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};
use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatCommand, ChatQuery, code};

use super::agent::Agent;
use super::{ChatKey, Engine};
use crate::chats::feed::Client;

/// The clients' way to the engine.
#[derive(Clone, Debug)]
pub(crate) struct Chats {
    asks: mpsc::Sender<Ask>,
    /// Whether the disk is full; it holds the latest, so no note is ever lost.
    disk_full: Arc<watch::Sender<bool>>,
}

pub(super) enum Ask {
    Command(ChatCommand, Client, oneshot::Sender<Result<(), Fault>>),
    Busy(oneshot::Sender<bool>),
    Leave(oneshot::Sender<bool>),
    /// An install the engine gave leave to is over.
    ResumeTurns,
    Query(
        ChatQuery,
        Client,
        oneshot::Sender<Result<Option<ChatAnswerKind>, Fault>>,
    ),
    /// A client hung up.
    Left,
}

fn stopped() -> Fault {
    Fault::new(code!("chat.engine_stopped"), "The daemon is stopping")
}

/// A way to the engine that lets it stop once every [`Chats`] is gone.
#[derive(Clone, Debug)]
pub(crate) struct WeakChats(mpsc::WeakSender<Ask>);

impl WeakChats {
    /// Whether an agent is on a turn, waits on a person, starts or is carried on by hand, or a
    /// session or a turn's end the journal couldn't save yet is only in memory, which a
    /// restart would cut off or lose.
    pub(crate) async fn busy(&self) -> Result<bool, Fault> {
        self.ask(Ask::Busy).await
    }

    /// Leave to install, given when nothing is busy and no other install holds it. Once given,
    /// no new turn starts and no chat can be held until [`WeakChats::resume_turns`].
    pub(crate) async fn leave(&self) -> Result<bool, Fault> {
        self.ask(Ask::Leave).await
    }

    async fn ask(&self, ask: fn(oneshot::Sender<bool>) -> Ask) -> Result<bool, Fault> {
        let asks = self.0.upgrade().ok_or_else(stopped)?;
        let (answer, answered) = oneshot::channel();
        asks.send(ask(answer)).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())
    }

    /// Lets turns go on after an install that had leave.
    pub(crate) async fn resume_turns(&self) {
        if let Some(asks) = self.0.upgrade() {
            drop(asks.send(Ask::ResumeTurns).await);
        }
    }
}

impl Chats {
    pub(super) fn new(asks: mpsc::Sender<Ask>, disk_full: watch::Sender<bool>) -> Self {
        let disk_full = Arc::new(disk_full);
        Self { asks, disk_full }
    }

    /// Acts on `command` from `client`. Its receipts are events in the chat's log; an error
    /// means it has no receipt, and sending it again is safe. A hold or a release has none: it
    /// is done once this returns.
    pub(crate) async fn command(&self, command: ChatCommand, client: &Client) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Command(command, client.clone(), answer);
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

    /// Tells the engine a client hung up, so a chat it held goes on at once.
    pub(crate) async fn left(&self) {
        drop(self.asks.send(Ask::Left).await);
    }

    /// Tells the engine whether the disk is full: while it is, prompts are queued and no new
    /// turn starts. Never waits, and only a change wakes the engine.
    pub(crate) fn disk_full(&self, full: bool) {
        self.disk_full
            .send_if_modified(|was| std::mem::replace(was, full) != full);
    }
}

impl<A: Agent> Engine<A> {
    // Answers `ask`, giving the chat it was about.
    pub(super) async fn answer(&mut self, ask: Ask) -> Option<ChatKey> {
        match ask {
            Ask::Command(command, client, answer) => {
                let key = ChatKey::new(&command.workspace, &command.chat);
                let done = self.command_from(&key, command, &client).await;
                drop(answer.send(self.checked(&key, done).await));
                Some(key)
            }
            Ask::Query(query, client, answer) => {
                let key = ChatKey::new(&query.workspace, &query.chat);
                let answered = self.query(&key, query.query, &client).await;
                drop(answer.send(self.checked(&key, answered).await));
                Some(key)
            }
            Ask::Busy(answer) => {
                self.answer_busy(answer).await;
                None
            }
            Ask::Leave(answer) => {
                self.answer_leave(answer).await;
                None
            }
            Ask::ResumeTurns => {
                self.resume_turns().await;
                None
            }
            Ask::Left => {
                self.close_unused().await;
                None
            }
        }
    }
}
