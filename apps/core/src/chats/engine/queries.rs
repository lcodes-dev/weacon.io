//! What clients ask about a chat's log.

use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatQueryKind, RunStatus};

use super::{Engine, Key, unsupported, update};
use crate::chats::feed::{Client, Subscriber};
use crate::runs::task::Agent;

const SNAPSHOT_EVENTS: u32 = 50;

impl<A: Agent> Engine<A> {
    pub(super) async fn query(
        &mut self,
        key: &Key,
        query: ChatQueryKind,
        client: &Client,
    ) -> Result<Option<ChatAnswerKind>, Fault> {
        if matches!(query, ChatQueryKind::Subscribe { .. }) {
            self.sweep();
        }
        let record = self.runs.of(&key.0, &key.1);
        let run = record.and_then(|record| Some((record.run.clone()?, record.state)));
        let chat = self.chat(key).await?;
        match query {
            ChatQueryKind::Subscribe { after } => {
                // A client has one subscription to a chat: its latest.
                chat.subscribers
                    .retain(|subscriber| !subscriber.is_for(client));
                let missed = chat.log.since(after).await?;
                let subscriber = Subscriber::new(client, &key.0, &key.1);
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
                seq: chat.log.seq(),
                runs: run
                    .map(|(run, state)| RunStatus {
                        run,
                        state,
                        agent: None,
                    })
                    .into_iter()
                    .collect(),
                waiting: chat.waiting.iter().map(|waiting| waiting.seq).collect(),
                events: chat.log.window(u64::MAX, SNAPSHOT_EVENTS).await?,
            })),
            ChatQueryKind::Window { before, limit } => Ok(Some(ChatAnswerKind::Window {
                events: chat.log.window(before, limit).await?,
            })),
            ChatQueryKind::Newer => Err(unsupported()),
        }
    }
}
