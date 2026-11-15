//! Whether the daemon may restart now, without cutting an agent off or losing what only
//! memory holds, and the pause in new turns while an install that may restart it has leave.

use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::Instant;
use weacon_protocol::RunState;

use super::{Agent, Engine};

/// How long leave lasts, and again from a reported restart: only an updater that hangs keeps
/// turns paused this long.
pub(crate) const LEAVE_LASTS: Duration = Duration::from_mins(2);

/// A leave the engine gave, by number, so word about an earlier one changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LeaveId(u64);

/// What the updater holding a leave says, or that it hung up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeaveStep {
    Restarting,
    Failed,
    HungUp,
}

/// The leaves given so far, and the one in force.
#[derive(Debug, Default)]
pub(crate) struct Install {
    given: u64,
    pub(super) pause: Option<Pause>,
}

#[derive(Debug)]
pub(super) struct Pause {
    leave: LeaveId,
    until: Instant,
    /// Once the daemon is about to restart, an updater that hangs up resumes nothing.
    restarting: bool,
}

impl Install {
    pub(super) const fn pauses(&self) -> bool {
        self.pause.is_some()
    }

    /// Ends once the leave in force runs out; never while there is none.
    pub(super) async fn runs_out(&self) {
        match &self.pause {
            Some(pause) => tokio::time::sleep_until(pause.until).await,
            None => std::future::pending().await,
        }
    }
}

impl<A: Agent> Engine<A> {
    // Saves what it can first, so a failed write doesn't keep it busy for good.
    pub(super) async fn answer_busy(&mut self, answer: oneshot::Sender<bool>) {
        self.save_pending().await;
        answer.send(self.busy()).unwrap_or_default();
    }

    // Gives leave when nothing is busy and no other install holds it; from then on no new turn
    // starts until the leave ends.
    pub(super) async fn answer_leave(&mut self, answer: oneshot::Sender<Option<LeaveId>>) {
        self.save_pending().await;
        if self.install.pauses() || self.busy() {
            return answer.send(None).unwrap_or_default();
        }
        self.install.given += 1;
        let leave = LeaveId(self.install.given);
        if answer.send(Some(leave)).is_ok() {
            let until = Instant::now() + LEAVE_LASTS;
            let restarting = false;
            self.install.pause = Some(Pause {
                leave,
                until,
                restarting,
            });
            self.log.info("turns paused for install", &[]);
        }
    }

    // Takes `step` of `leave`, if it is the leave in force; answers whether it was.
    pub(super) async fn answer_step(
        &mut self,
        leave: LeaveId,
        step: LeaveStep,
        answer: oneshot::Sender<bool>,
    ) {
        let pause = self
            .install
            .pause
            .as_mut()
            .filter(|pause| pause.leave == leave && pause.until > Instant::now());
        let Some(pause) = pause else {
            return answer.send(false).unwrap_or_default();
        };
        answer.send(true).unwrap_or_default();
        match step {
            LeaveStep::Restarting => {
                pause.restarting = true;
                pause.until = Instant::now() + LEAVE_LASTS;
            }
            LeaveStep::HungUp if pause.restarting => {}
            LeaveStep::Failed | LeaveStep::HungUp => self.resume_turns().await,
        }
    }

    async fn resume_turns(&mut self) {
        if self.install.pause.take().is_some() {
            self.turns_resumed().await;
        }
    }

    pub(super) async fn turns_resumed(&mut self) {
        self.log.info("turns resumed after install", &[]);
        self.go_on().await;
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
        assert!(by_hand.chats.downgrade().busy().await.unwrap());
        assert!(by_hand.chats.downgrade().leave().await.unwrap().is_none());

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
    async fn with_leave_to_install_prompts_queue_holds_are_refused_and_turns_go_on_after() {
        let mut bench = bench();
        let installs = bench.chats.downgrade();
        let leave = installs.leave().await.unwrap().unwrap();
        let other = installs.leave().await.unwrap();
        assert!(other.is_none(), "one install at a time");
        assert!(!installs.busy().await.unwrap(), "leave alone isn't busy");

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        let (holder, _pushes) = client(1);
        let hold = command("h-1", "c-1", ChatAction::Hold);
        let refused = bench.chats.command(hold, &holder).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.busy");
        assert!(soon(bench.started.recv()).await.is_none());

        assert!(installs.step(leave, LeaveStep::Failed).await.unwrap());
        assert!(soon(bench.started.recv()).await.is_some());
        let logged = messages(&bench.records);
        let paused = ["turns paused for install", "turns resumed after install"];
        assert!(
            paused.iter().all(|event| logged.contains(event)),
            "{logged:?}"
        );
    }

    // Asks the engine for leave, as an asker that is gone already when `gone`.
    async fn asked(engine: &mut Engine<FakeAgent>, gone: bool) {
        let (answer, answered) = oneshot::channel();
        if gone {
            drop(answered);
        }
        engine.answer_leave(answer).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_busy_engine_or_an_asker_gone_gives_no_leave() {
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
    async fn word_of_a_leave_that_ran_out_changes_nothing() {
        let mut bench = bench();
        let installs = bench.chats.downgrade();
        let first = installs.leave().await.unwrap().unwrap();
        tokio::time::sleep(LEAVE_LASTS + Duration::from_secs(1)).await;
        let second = installs.leave().await.unwrap().unwrap();
        assert_ne!(first, second);
        for step in [LeaveStep::Restarting, LeaveStep::Failed, LeaveStep::HungUp] {
            assert!(!installs.step(first, step).await.unwrap());
        }
        bench.command(prompt("r-1", "Hi")).await.unwrap();
        assert!(soon(bench.started.recv()).await.is_none(), "still paused");
        assert!(installs.step(second, LeaveStep::HungUp).await.unwrap());
        assert!(soon(bench.started.recv()).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_restart_outlasts_a_hang_up_and_runs_out_from_when_it_was_said() {
        let mut bench = bench();
        let installs = bench.chats.downgrade();
        let leave = installs.leave().await.unwrap().unwrap();
        bench.command(prompt("r-1", "Hi")).await.unwrap();
        tokio::time::sleep(LEAVE_LASTS / 2).await;
        assert!(installs.step(leave, LeaveStep::Restarting).await.unwrap());
        assert!(installs.step(leave, LeaveStep::HungUp).await.unwrap());
        tokio::time::sleep(LEAVE_LASTS * 3 / 4).await;
        assert!(soon(bench.started.recv()).await.is_none());
        tokio::time::sleep(LEAVE_LASTS / 4).await;
        assert!(soon(bench.started.recv()).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_comes_after_its_leave_ran_out_is_refused() {
        let (_bench, mut engine) = parts();
        let (answer, answered) = oneshot::channel();
        engine.answer_leave(answer).await;
        let leave = answered.await.unwrap().unwrap();
        // The engine's loop isn't running, as when it was held up past the deadline.
        tokio::time::sleep(LEAVE_LASTS).await;
        let (answer, answered) = oneshot::channel();
        engine
            .answer_step(leave, LeaveStep::Restarting, answer)
            .await;
        assert!(!answered.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn asking_whether_it_is_busy_pauses_nothing() {
        let (_bench, mut engine) = parts();
        let (answer, answered) = oneshot::channel();
        engine.answer_busy(answer).await;
        assert!(!answered.await.unwrap());
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        assert!(engine.is_launching(&key()));
    }
}
