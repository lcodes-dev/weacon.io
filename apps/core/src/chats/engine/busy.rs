//! Whether the daemon may restart now, without cutting an agent off or losing what only
//! memory holds.

use tokio::sync::oneshot;
use weacon_protocol::RunState;

use super::{Agent, Engine};

impl<A: Agent> Engine<A> {
    // Saves what it can first, so one failed write doesn't keep the engine busy for good.
    pub(super) async fn answer_busy(&mut self, answer: oneshot::Sender<bool>) {
        self.save_pending_sessions().await;
        // An asker that's gone needs no answer.
        answer.send(self.busy()).unwrap_or_default();
    }

    pub(super) fn busy(&self) -> bool {
        self.open_chats.iter().any(|(key, chat)| {
            let live = chat.orders.is_some()
                && self.runs.of(key).is_some_and(|record| {
                    record.turn.is_some() || record.state == RunState::Waiting
                });
            live || chat.pending_session.is_some() || chat.pending_end.is_some()
        })
    }
}
