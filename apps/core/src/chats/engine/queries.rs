//! What clients ask about a chat's log.

use std::path::PathBuf;

use weacon_observability::Fault;
use weacon_protocol::{AgentKind, ChatAnswerKind, ChatQueryKind, RunState};

use super::agent::Agent;
use super::{ChatKey, Engine, unsupported, update};
use crate::chats::feed::{Client, Subscriber};

const SNAPSHOT_EVENTS: u32 = 50;

/// A chat the engine has a record of: its workspace's folder, its agent, its latest run's
/// state once it has had one, and when its latest turn began or ended, in Unix seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) key: ChatKey,
    pub(crate) root: PathBuf,
    pub(crate) agent: AgentKind,
    pub(crate) run: Option<RunState>,
    pub(crate) active_at: u64,
}

impl<A: Agent> Engine<A> {
    pub(super) fn listed(&self) -> Vec<Listed> {
        self.runs
            .records()
            .map(|record| Listed {
                key: record.key.clone(),
                root: self.workspaces.root(&record.key.workspace),
                agent: record.agent,
                run: record.run.as_ref().map(|_| record.state),
                active_at: record.active_at,
            })
            .collect()
    }

    pub(super) async fn query(
        &mut self,
        key: &ChatKey,
        query: ChatQueryKind,
        client: &Client,
    ) -> Result<Option<ChatAnswerKind>, Fault> {
        // A client that hung up is noticed only when told something, so each subscription first
        // closes chats only gone clients watched, before opening one more.
        if matches!(query, ChatQueryKind::Subscribe { .. }) {
            self.close_unused().await;
        }
        let chat = self.open(key).await?;
        match query {
            ChatQueryKind::Subscribe { after } => {
                // A client has one subscription to a chat: its latest.
                chat.subscribers
                    .retain(|subscriber| !subscriber.client().is(client));
                let missed = chat.log.since(after).await?;
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
                seq: chat.log.seq(),
                runs: chat.log.runs().run().cloned().into_iter().collect(),
                waiting: chat.log.runs().waiting_seqs(),
                events: chat.log.window(u64::MAX, SNAPSHOT_EVENTS).await?,
            })),
            ChatQueryKind::Window { before, limit } => Ok(Some(ChatAnswerKind::Window {
                events: chat.log.window(before, limit).await?,
            })),
            ChatQueryKind::Newer => Err(unsupported()),
        }
    }
}
