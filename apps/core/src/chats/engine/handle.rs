//! The clients' way to the engine, and how the engine answers them.

use tokio::sync::{mpsc, oneshot};
use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatCommand, ChatQuery, code};

use super::agent::Agent;
use super::queries::Listed;
use super::{ChatKey, Engine};
use crate::chats::feed::Client;

/// The clients' way to the engine.
#[derive(Clone, Debug)]
pub(crate) struct Chats {
    asks: mpsc::Sender<Ask>,
}

pub(super) enum Ask {
    Command(Box<ChatCommand>, Client, oneshot::Sender<Result<(), Fault>>),
    Query(
        ChatQuery,
        Client,
        oneshot::Sender<Result<Option<ChatAnswerKind>, Fault>>,
    ),
    List(oneshot::Sender<Vec<Listed>>),
    Rest(ChatKey, oneshot::Sender<Result<(), Fault>>),
    /// A client hung up.
    Left,
}

fn stopped() -> Fault {
    Fault::new(code!("chat.engine_stopped"), "The daemon is stopping")
}

impl Chats {
    pub(super) const fn new(asks: mpsc::Sender<Ask>) -> Self {
        Self { asks }
    }

    /// Acts on `command` from `client`. Its receipts are events in the chat's log; an error
    /// refuses it. A hold or a release has none: it is done once this returns.
    pub(crate) async fn command(&self, command: ChatCommand, client: &Client) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Command(Box::new(command), client.clone(), answer);
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

    /// Every chat the engine has a record of, in no order.
    pub(crate) async fn list(&self) -> Result<Vec<Listed>, Fault> {
        let (answer, answered) = oneshot::channel();
        self.asks
            .send(Ask::List(answer))
            .await
            .map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())
    }

    /// Stops the chat's agent between turns, so its next prompt starts it again in its folder;
    /// `worktree.busy` while it is on a turn.
    pub(crate) async fn rest(&self, key: ChatKey) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Rest(key, answer);
        self.asks.send(ask).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())?
    }

    /// Tells the engine a client hung up, so a chat it held goes on at once.
    pub(crate) async fn left(&self) {
        drop(self.asks.send(Ask::Left).await);
    }
}

impl<A: Agent> Engine<A> {
    // Answers `ask`, giving the chat it was about.
    pub(super) async fn answer(&mut self, ask: Ask) -> Option<ChatKey> {
        match ask {
            Ask::Command(command, client, answer) => {
                let key = ChatKey::new(&command.workspace, &command.chat);
                let done = self.command_from(&key, *command, &client).await;
                drop(answer.send(done));
                Some(key)
            }
            Ask::Query(query, client, answer) => {
                let key = ChatKey::new(&query.workspace, &query.chat);
                let answered = self.query(&key, query.query, &client).await;
                drop(answer.send(answered));
                Some(key)
            }
            Ask::List(answer) => {
                drop(answer.send(self.listed()));
                None
            }
            Ask::Rest(key, answer) => {
                drop(answer.send(self.rest(&key).await));
                None
            }
            Ask::Left => {
                self.close_unused().await;
                None
            }
        }
    }
}
