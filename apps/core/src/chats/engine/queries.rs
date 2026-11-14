//! What clients ask about a chat's log.

use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatQueryKind};

use super::agent::Agent;
use super::{ChatKey, Engine, unsupported, update};
use crate::chats::feed::{Client, Subscriber};

const SNAPSHOT_EVENTS: u32 = 50;

impl<A: Agent> Engine<A> {
    pub(super) async fn query(
        &mut self,
        key: &ChatKey,
        query: ChatQueryKind,
        client: &Client,
    ) -> Result<Option<ChatAnswerKind>, Fault> {
        // A client that hung up is noticed only when told something, so each subscription first
        // closes chats only gone clients watched, before opening one more.
        if matches!(query, ChatQueryKind::Subscribe { .. }) {
            self.sweep();
        }
        // A run cut off, as by a restart, isn't shown as working.
        self.save_ended(key).await?;
        let chat = self.open(key).await?;
        match query {
            ChatQueryKind::Subscribe { after } => {
                // A client has one subscription to a chat: its latest.
                chat.subscribers
                    .retain(|subscriber| !subscriber.is_for(client));
                let missed = chat.events.since(after).await?;
                let subscriber = Subscriber::new(client, key);
                let (Some(missed), Some(subscriber)) = (missed, subscriber) else {
                    return Ok(Some(ChatAnswerKind::EventsLost));
                };
                let caught_up = missed
                    .into_iter()
                    .try_fold(subscriber, |subscriber, entry| {
                        subscriber.tell(update(key, entry))
                    });
                chat.subscribers.extend(caught_up);
                Ok(None)
            }
            ChatQueryKind::Snapshot => Ok(Some(ChatAnswerKind::Snapshot {
                seq: chat.events.seq(),
                runs: chat.events.runs().runs.clone(),
                waiting: chat.events.runs().waiting_seqs(),
                events: chat.events.window(u64::MAX, SNAPSHOT_EVENTS).await?,
            })),
            ChatQueryKind::Window { before, limit } => Ok(Some(ChatAnswerKind::Window {
                events: chat.events.window(before, limit).await?,
            })),
            ChatQueryKind::Newer => Err(unsupported()),
        }
    }
}
