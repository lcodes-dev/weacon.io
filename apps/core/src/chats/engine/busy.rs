//! Whether the daemon may restart now, without cutting an agent off or losing what only
//! memory holds, and the pause in new turns while an install may restart it.

use tokio::sync::oneshot;
use weacon_protocol::RunState;

use super::{Agent, Engine};

impl<A: Agent> Engine<A> {
    // Saves what it can first, so a failed write doesn't keep it busy for good. When nothing
    // is busy and the asker is there to install, no new turn starts until `resume_turns`.
    pub(super) async fn answer_busy(&mut self, answer: oneshot::Sender<bool>) {
        self.save_pending().await;
        let busy = self.busy();
        if answer.send(busy).is_err() || busy {
            return self.resume_turns().await;
        }
        if !std::mem::replace(&mut self.installing, true) {
            self.log.info("turns paused for install", &[]);
        }
    }

    pub(super) async fn resume_turns(&mut self) {
        if std::mem::take(&mut self.installing) {
            self.log.info("turns resumed after install", &[]);
            self.go_on().await;
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.open_chats.iter().any(|(key, chat)| {
            let live = chat.orders.is_some()
                && self.runs.of(key).is_some_and(|record| {
                    record.turn.is_some() || record.state == RunState::Waiting
                });
            live || chat.is_starting()
                || self.is_held(key)
                || chat.pending_session.is_some()
                || chat.pending_end.is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;
    use weacon_protocol::ChatAction;

    use weacon_testing::messages;

    use super::super::handle::Ask;
    use super::*;
    use crate::chats::test_support::{
        FakeAgent, bench, chat, client, command, key, parts, prompt, someone, soon,
    };
    use crate::runs::ChatKey;

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
    async fn while_an_install_may_run_prompts_queue_holds_are_refused_and_turns_go_on_after() {
        let mut bench = bench();
        let installs = bench.chats.downgrade();
        assert!(!installs.pause_unless_busy().await.unwrap());

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        let (holder, _pushes) = client(1);
        let hold = command("h-1", "c-1", ChatAction::Hold);
        let refused = bench.chats.command(hold, &holder).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.busy");
        assert!(soon(bench.started.recv()).await.is_none());

        installs.resume_turns().await;
        assert!(soon(bench.started.recv()).await.is_some());
        let logged = messages(&bench.records);
        let paused = ["turns paused for install", "turns resumed after install"];
        assert!(
            paused.iter().all(|event| logged.contains(event)),
            "{logged:?}"
        );
    }

    // Asks the engine whether it is busy, as an asker that is gone already when `gone`.
    async fn asked(engine: &mut Engine<FakeAgent>, gone: bool) {
        let (answer, answered) = oneshot::channel();
        if gone {
            drop(answered);
        }
        engine.answer_busy(answer).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_busy_engine_or_an_asker_gone_pauses_no_turn() {
        let (_bench, mut engine) = parts();
        let other = ChatKey {
            chat: chat("c-2"),
            ..key()
        };
        engine
            .command(&other, command("r-1", "c-2", prompt("r-1", "Hi").action))
            .await
            .unwrap();
        asked(&mut engine, false).await;
        engine.command(&key(), prompt("r-2", "Hi")).await.unwrap();
        assert!(engine.is_launching(&key()), "paused while busy");

        let (_bench, mut idle) = parts();
        asked(&mut idle, true).await;
        idle.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        assert!(idle.is_launching(&key()), "paused for an asker gone");
    }

    #[tokio::test(start_paused = true)]
    async fn a_pause_whose_resume_was_lost_ends_at_the_next_answer_that_starts_no_install() {
        let (_bench, mut engine) = parts();
        asked(&mut engine, false).await;
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        assert!(!engine.is_launching(&key()));

        asked(&mut engine, true).await;

        assert!(engine.is_launching(&key()));
    }
}
