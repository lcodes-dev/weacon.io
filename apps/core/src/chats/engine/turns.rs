//! What the engine does with a chat's turns. A prompt is logged, queued in the journal and
//! accepted, then given its turn, which ends with a receipt. Each step is saved before the next;
//! `advance` finishes what a failed write or a restart left half done.

use weacon_observability::Fault;
use weacon_protocol::{Outcome, RunId, RunState, RunStatus};

use super::agent::{Agent, Order};
use super::{ChatKey, Engine, PendingEnd};
use crate::runs::Queued;

impl<A: Agent> Engine<A> {
    // Ends the turn, if one is running, with `outcome`; the run is left idle, or asleep once its
    // agent is gone.
    pub(super) async fn end_turn(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        outcome: Outcome,
    ) -> Result<(), Fault> {
        let (turn, now) = (self.turn(key), self.now());
        let state = if self.is_live(key) {
            RunState::Idle
        } else {
            RunState::Sleeping
        };
        let chat = self.open(key).await?;
        chat.pending_answers.clear();
        // What the run reported first wins over a later end, such as its exit.
        let outcome = match chat.pending_end.take() {
            Some(end) if turn.as_ref() == Some(&end.turn) => end.outcome,
            _ => outcome,
        };
        // The turn stays in the journal until its receipt is in the log; `advance` retries it.
        if let Some(request) = turn
            && let Err(fault) = self.commit_receipt(key, &request, outcome.clone()).await
        {
            if let Some(chat) = self.open_chats.get_mut(key) {
                chat.pending_end = Some(PendingEnd {
                    turn: request,
                    outcome,
                });
            }
            return Err(fault);
        }
        // The state goes in the same save, so no record is left running with no turn.
        self.runs
            .update(key, |record| {
                record.turn = None;
                record.state = state;
                record.active_at = now;
            })
            .await?;
        self.tell_state(key, run, state).await
    }

    // The agent is gone: its turn ends unknown, and what is queued goes to the chat's next run.
    pub(super) async fn exited(&mut self, key: &ChatKey, run: &RunId) -> Result<(), Fault> {
        self.open(key).await?.orders = None;
        self.end_turn(key, run, Outcome::Unknown).await?;
        self.advance(key).await
    }

    // Starts the next queued prompt's turn, in a new run when the chat's agent isn't running.
    pub(super) async fn advance(&mut self, key: &ChatKey) -> Result<(), Fault> {
        self.save_ended(key).await?;
        let Some(record) = self.runs.of(key) else {
            return Ok(());
        };
        // A turn whose agent is gone is over, even one `save_ended` couldn't end for want of a run.
        if record.turn.is_some() && self.is_live(key) {
            return Ok(());
        }
        let Some(next) = record.queue.first().cloned() else {
            return Ok(());
        };
        let run = match self.run_of(key).filter(|_| self.takes_orders(key)) {
            Some(run) => run,
            None => match self.launch(key).await {
                Ok(run) => run,
                Err(fault) => return self.fail_queue(key, &fault).await,
            },
        };
        self.give_turn(key, &run, next).await
    }

    // Moves `next` from the queue to the turn and sends it to `run`.
    async fn give_turn(&mut self, key: &ChatKey, run: &RunId, next: Queued) -> Result<(), Fault> {
        let now = self.now();
        self.runs
            .update(key, |record| {
                record.queue.retain(|queued| queued.request != next.request);
                record.turn = Some(next.request.clone());
                record.active_at = now;
            })
            .await?;
        if !self.order(key, Order::Send(next.text.clone())) {
            // It stopped reading while the journal was saved: the prompt waits for the next run.
            return self
                .runs
                .update(key, |record| {
                    record.turn = None;
                    record.queue.insert(0, next);
                })
                .await;
        }
        self.set_state(key, run, RunState::Running).await
    }

    // Saves the ends a failed write or an exit left unsaved: the turn's, and those of queued
    // prompts, which then never run.
    pub(super) async fn save_ended(&mut self, key: &ChatKey) -> Result<(), Fault> {
        let Some(record) = self.runs.of(key) else {
            return Ok(());
        };
        let (turn, queue) = (record.turn.clone(), record.queue.clone());
        let live = self.is_live(key);
        let chat = self.open(key).await?;
        // A turn whose run is gone, as after a restart, is over too, and so is that run.
        let turn_ended = turn.is_some_and(|turn| {
            !live || chat.pending_end.is_some() || chat.events.has_ended(&turn)
        });
        let working = |status: &RunStatus| {
            matches!(
                status.state,
                RunState::Starting | RunState::Running | RunState::Waiting
            )
        };
        let cut_off = !live && chat.events.runs().runs.iter().any(working);
        let ended: Vec<_> = queue
            .into_iter()
            .map(|queued| queued.request)
            .filter(|request| chat.events.has_ended(request))
            .collect();
        if !ended.is_empty() {
            self.runs
                .update(key, |record| {
                    record
                        .queue
                        .retain(|queued| !ended.contains(&queued.request));
                })
                .await?;
        }
        match self.run_of(key) {
            Some(run) if turn_ended || cut_off => self.end_turn(key, &run, Outcome::Unknown).await,
            _ => Ok(()),
        }
    }

    // Nothing can run them: each queued prompt fails rather than waits for ever.
    async fn fail_queue(&mut self, key: &ChatKey, fault: &Fault) -> Result<(), Fault> {
        let record = self.runs.of(key);
        let queue = record
            .map(|record| record.queue.clone())
            .unwrap_or_default();
        // Each one leaves the queue only once its receipt is in the log.
        for queued in queue {
            let code = fault.code().clone();
            self.commit_receipt(key, &queued.request, Outcome::Failed { code })
                .await?;
        }
        self.save_ended(key).await
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use weacon_protocol::{ChatEvent, Receipt, RequestId, RunEvent, code};

    use super::super::agent::Heard;
    use super::super::by_hand::{hear, latest, on_ask, on_turn, until_deaf, waiting_in};
    use super::*;
    use crate::chats::test_support::{FakeAgent, Told, key, prompt, run, soon};

    // The outcomes of `request`'s receipts in the log, in order.
    async fn outcomes(engine: &mut Engine<FakeAgent>, request: &RequestId) -> Vec<Outcome> {
        let log = &engine.open(&key()).await.unwrap().events;
        let window = log.window(u64::MAX, 64).await.unwrap();
        window
            .into_iter()
            .filter_map(|entry| receipt_of(entry.event))
            .filter(|receipt| receipt.request == *request)
            .map(|receipt| receipt.outcome)
            .collect()
    }

    fn receipt_of(event: ChatEvent) -> Option<Receipt> {
        let ChatEvent::Receipt { receipt } = event else {
            return None;
        };
        Some(receipt)
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_ends_even_when_its_last_event_cant_be_committed() {
        let mut on = on_turn().await;
        on.engine.open(&key()).await.unwrap().events.jam();

        let outcome = Outcome::Completed;
        let lost = hear(&mut on.engine, RunEvent::Finished { outcome }).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(on.engine.turn(&key()), None);
        let idle = ChatEvent::RunStatus {
            status: RunStatus {
                run: run("run-1"),
                state: RunState::Idle,
                agent: None,
            },
        };
        assert_eq!(latest(&mut on.engine).await, idle);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_agent_is_gone_doesnt_hold_up_the_next_prompt() {
        let mut on = on_turn().await;
        // As when the agent exited and the journal couldn't be told the turn was over.
        on.engine.open(&key()).await.unwrap().orders = None;

        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();

        let mut next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_that_stopped_reading_gets_no_turn_and_the_prompt_goes_to_a_new_run() {
        let mut on = on_turn().await;
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        // The agent exits; its report of that isn't handled yet.
        drop(on.agent.say);
        until_deaf(&on.engine).await;

        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        let mut next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        // The old run's exit, heard late, leaves the new one alone.
        let exited = Heard::Exited(None);
        on.engine
            .heard(&key(), &run("run-1"), exited)
            .await
            .unwrap();
        assert!(on.engine.takes_orders(&key()));
        assert_eq!(on.engine.turn(&key()), Some(prompt("r-2", "More").request));
    }

    #[tokio::test(start_paused = true)]
    async fn a_working_run_with_no_turn_left_after_a_restart_sleeps() {
        let mut on = on_turn().await;
        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        // As when the core stopped right after starting a run, before it got a turn.
        let status = RunStatus {
            run: run("run-1"),
            state: RunState::Running,
            agent: None,
        };
        on.engine
            .commit(&key(), ChatEvent::RunStatus { status })
            .await
            .unwrap();
        on.engine.open_chats.remove(&key());

        waiting_in(&mut on.engine).await;

        let status = RunStatus {
            run: run("run-1"),
            state: RunState::Sleeping,
            agent: None,
        };
        assert_eq!(
            latest(&mut on.engine).await,
            ChatEvent::RunStatus { status }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_cut_off_by_a_restart_ends_and_leaves_no_prompt_waiting() {
        let mut on = on_ask().await;
        assert_eq!(waiting_in(&mut on.engine).await.len(), 1);
        // As after a restart: the log is all that is left of the chat.
        on.engine.open_chats.remove(&key());

        assert!(waiting_in(&mut on.engine).await.is_empty());
        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut on.engine, &first).await,
            [Outcome::Accepted, Outcome::Unknown]
        );
        let status = RunStatus {
            run: run("run-1"),
            state: RunState::Sleeping,
            agent: None,
        };
        assert_eq!(
            latest(&mut on.engine).await,
            ChatEvent::RunStatus { status }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_receipt_cant_be_committed_ends_as_reported_on_the_next_prompt() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let request = prompt("r-1", "Hi").request;
        on.engine.open(&key()).await.unwrap().events.jam();
        let run_1 = run("run-1");
        let lost = on.engine.end_turn(&key(), &run_1, Outcome::Completed).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(on.engine.turn(&key()), Some(request.clone()));

        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();

        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        assert_eq!(
            outcomes(&mut on.engine, &request).await,
            [Outcome::Accepted, Outcome::Completed]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_prompt_whose_end_is_in_the_log_never_runs() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        // As when its failure was committed but it couldn't leave the queue.
        let code = code!("agent.not_started");
        let request = prompt("r-2", "More").request;
        on.engine
            .commit_receipt(&key(), &request, Outcome::Failed { code })
            .await
            .unwrap();

        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
        assert!(on.engine.runs.of(&key()).unwrap().queue.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_exit_whose_receipt_failed_is_ended_before_the_next_run_starts() {
        let mut on = on_turn().await;
        on.engine.open(&key()).await.unwrap().events.jam();
        drop(on.agent);
        let exited = Heard::Exited(None);
        let lost = on.engine.heard(&key(), &run("run-1"), exited).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        // Its outcome keeps the chat open, though nothing else does.
        on.engine.settle(&key());
        assert!(on.engine.open_chats.contains_key(&key()));

        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();

        let mut next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut on.engine, &first).await,
            [Outcome::Accepted, Outcome::Unknown]
        );
        assert!(on.engine.open(&key()).await.unwrap().pending_end.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn an_outcome_left_by_another_turn_doesnt_end_this_one() {
        let mut on = on_turn().await;
        let other = PendingEnd {
            turn: prompt("r-0", "Earlier").request,
            outcome: Outcome::Completed,
        };
        on.engine.open(&key()).await.unwrap().pending_end = Some(other);

        let run_1 = run("run-1");
        on.engine
            .end_turn(&key(), &run_1, Outcome::Interrupted)
            .await
            .unwrap();

        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut on.engine, &first).await,
            [Outcome::Accepted, Outcome::Interrupted]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_run_is_gone_ends_unknown_on_the_next_prompt() {
        let mut on = on_turn().await;
        // As after a restart: nothing of the chat or its run is in memory.
        on.engine.open_chats.remove(&key());

        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();

        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut on.engine, &first).await,
            [Outcome::Accepted, Outcome::Unknown]
        );
        let mut next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_its_run_never_got_goes_back_to_the_queue() {
        let mut on = on_turn().await;
        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        // The run stopped reading while its next turn was being saved.
        let (closed, gone) = mpsc::unbounded_channel();
        drop(gone);
        on.engine.open(&key()).await.unwrap().orders = Some(closed);
        let next = Queued {
            request: prompt("r-2", "More").request,
            text: "More".to_owned(),
        };

        on.engine
            .give_turn(&key(), &run("run-1"), next.clone())
            .await
            .unwrap();

        let record = on.engine.runs.of(&key()).unwrap();
        assert_eq!(
            (record.turn.as_ref(), record.queue.as_slice()),
            (None, [next].as_slice())
        );
    }
}
