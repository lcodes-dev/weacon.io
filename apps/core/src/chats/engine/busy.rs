//! Whether the daemon may restart now, without cutting an agent off or losing what only
//! memory holds, and the pause in new turns while an install may restart it.

use tokio::sync::oneshot;
use weacon_protocol::RunState;

use super::{Agent, Engine};

impl<A: Agent> Engine<A> {
    // Saves what it can first, so one failed write doesn't keep the engine busy for good. When
    // nothing is busy, no new turn starts until `go_on_after_install`; an asker that's gone
    // starts no install.
    pub(super) async fn answer_busy(&mut self, answer: oneshot::Sender<bool>) {
        self.save_pending().await;
        let busy = self.busy();
        self.installing = answer.send(busy).is_ok() && !busy;
    }

    pub(super) async fn go_on_after_install(&mut self) {
        if std::mem::take(&mut self.installing) {
            self.go_on().await;
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.open_chats.iter().any(|(key, chat)| {
            let live = chat.orders.is_some()
                && self.runs.of(key).is_some_and(|record| {
                    record.turn.is_some() || record.state == RunState::Waiting
                });
            // A hand's agent or one still starting runs as well.
            let held = chat.held.as_ref().is_some_and(|holder| !holder.is_gone());
            live || chat.is_starting()
                || held
                || chat.pending_session.is_some()
                || chat.pending_end.is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;
    use weacon_protocol::ChatAction;

    use super::super::handle::Ask;
    use crate::chats::test_support::{bench, client, command, key, parts, prompt, someone, soon};

    #[tokio::test(start_paused = true)]
    async fn a_chat_held_by_hand_or_an_agent_let_go_while_it_starts_is_busy() {
        let by_hand = bench();
        let (holder, _pushes) = client(1);
        let hold = command("h-1", "c-1", ChatAction::Hold);
        by_hand.chats.command(hold, &holder).await.unwrap();
        assert!(by_hand.chats.downgrade().pause_unless_busy().await.unwrap());

        let (_bench, mut engine) = parts();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        engine.open(&key()).await.unwrap().log.jam();
        let (answer, answered) = oneshot::channel();
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        engine
            .answer(Ask::Command(interrupt, someone(), answer))
            .await;
        answered.await.unwrap().unwrap_err();
        assert!(engine.busy(), "an agent let go is still starting");
    }

    #[tokio::test(start_paused = true)]
    async fn while_an_install_may_run_prompts_queue_and_go_on_once_it_is_over() {
        let mut bench = bench();
        let installs = bench.chats.downgrade();
        assert!(!installs.pause_unless_busy().await.unwrap());

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        let (holder, _pushes) = client(1);
        let (hold, release) = (ChatAction::Hold, ChatAction::Release);
        bench
            .chats
            .command(command("h-1", "c-1", hold), &holder)
            .await
            .unwrap();
        bench
            .chats
            .command(command("h-2", "c-1", release), &holder)
            .await
            .unwrap();
        assert!(soon(bench.started.recv()).await.is_none());

        installs.installed().await;
        assert!(soon(bench.started.recv()).await.is_some());
    }
}
