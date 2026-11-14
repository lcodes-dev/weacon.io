//! Whether the daemon may restart now, without cutting an agent off or losing what only
//! memory holds.

use tokio::sync::oneshot;
use weacon_observability::Fault;
use weacon_protocol::RunState;

use super::{Agent, Ask, Chats, Engine, stopped};

impl Chats {
    /// Whether an agent is on a turn or waits on a person, or a session or a turn's end the
    /// journal couldn't save yet is only in memory, which a restart would cut off or lose.
    pub(crate) async fn busy(&self) -> Result<bool, Fault> {
        let (answer, answered) = oneshot::channel();
        self.asks
            .send(Ask::Busy(answer))
            .await
            .map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())
    }
}

impl<A: Agent> Engine<A> {
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
