//! Who is told about a chat's events as they are committed.

use tokio::sync::mpsc::{self, error::TrySendError};
use weacon_protocol::{ChatAnswer, ChatAnswerKind, ChatId, Message, WorkspaceId};

/// How many pushed messages may wait for one client; one more than it can miss.
pub(crate) const QUEUE: usize = 1024;

/// Where the daemon pushes messages to one connected client.
#[derive(Clone, Debug)]
pub(crate) struct Client {
    pub(crate) push: mpsc::Sender<Message>,
}

/// One client's subscription to one chat.
#[derive(Debug)]
pub(crate) struct Subscriber {
    push: mpsc::Sender<Message>,
    // A place in the client's queue kept free, so it can always be told it missed events.
    lost: mpsc::OwnedPermit<Message>,
    notice: Message,
}

impl Subscriber {
    /// `None` when the client's queue is full or it is gone.
    pub(crate) fn new(client: &Client, workspace: &WorkspaceId, chat: &ChatId) -> Option<Self> {
        let notice = Message::ChatAnswer(ChatAnswer {
            workspace: workspace.clone(),
            chat: chat.clone(),
            answer: ChatAnswerKind::EventsLost,
        });
        Some(Self {
            push: client.push.clone(),
            lost: client.push.clone().try_reserve_owned().ok()?,
            notice,
        })
    }

    pub(crate) fn is_for(&self, client: &Client) -> bool {
        self.push.same_channel(&client.push)
    }

    pub(crate) fn is_gone(&self) -> bool {
        self.push.is_closed()
    }

    /// Queues `message`, or ends the subscription by telling the client it missed events;
    /// `None` once the subscription is over.
    pub(crate) fn tell(self, message: Message) -> Option<Self> {
        match self.push.try_send(message) {
            Ok(()) => Some(self),
            Err(TrySendError::Full(_)) => {
                self.end();
                None
            }
            Err(TrySendError::Closed(_)) => None,
        }
    }

    /// Ends the subscription by telling the client it missed events.
    pub(crate) fn end(self) {
        self.lost.send(self.notice);
    }
}
