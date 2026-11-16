//! A write that fails, to a chat's log or to the runs journal, ends that chat's run, so the
//! run on disk never disagrees with what happens. Other chats go on.

use weacon_observability::Fault;
use weacon_protocol::{Outcome, RunId, code};

use super::agent::{Agent, FromRun};
use super::launch::Launched;
use super::{ChatKey, Engine, failed};

impl<A: Agent> Engine<A> {
    // Passes `result` on, ending the chat's run first when it is a failed write.
    pub(super) async fn checked<T>(
        &mut self,
        key: &ChatKey,
        result: Result<T, Fault>,
    ) -> Result<T, Fault> {
        if let Err(fault) = &result {
            self.end_run(key, None, fault).await;
        }
        result
    }

    // As `checked`, for the engine's own work, about the run `of` when it is a run's: the fault
    // still to be logged, none once ending the chat's run has logged it.
    pub(super) async fn unlogged(
        &mut self,
        key: &ChatKey,
        of: Option<&RunId>,
        result: Result<(), Fault>,
    ) -> Option<Fault> {
        let fault = result.err()?;
        (!self.end_run(key, of, &fault).await).then_some(fault)
    }

    // On a failed write, tells the chat's agent to stop and ends its run failed where that can
    // still be written; a turn whose end can't be is ended by the chat's next `advance`.
    // A fault of a run that is no longer the chat's ends nothing. Whether it ended a run, which
    // it logs.
    async fn end_run(&mut self, key: &ChatKey, of: Option<&RunId>, fault: &Fault) -> bool {
        let current = |run: &RunId| is_unwritten(fault) && of.is_none_or(|of| of == run);
        let Some(run) = self.run_of(key).filter(current) else {
            return false;
        };
        let Some(chat) = self.open_chats.get_mut(key) else {
            return false;
        };
        // The agent stops once its driver reads its orders again, after any it is writing.
        let stopped = chat.orders.take().is_some();
        let starting = chat.launching.take();
        if !stopped && starting.is_none() {
            return false;
        }
        if starting.is_some() {
            chat.let_go = starting;
        }
        chat.pending_interrupts.clear();
        self.log
            .warn("run ended unwritten", &failed(key, &run, fault));
        let code = fault.code().clone();
        drop(self.end_turn(key, &run, Outcome::Failed { code }).await);
        true
    }

    // Takes up what a run said; the chat it is about.
    pub(super) async fn take_said(&mut self, from: FromRun) -> ChatKey {
        let FromRun { key, run, said } = from;
        let heard = self.hear(&key, &run, said).await;
        if let Some(fault) = self.unlogged(&key, Some(&run), heard).await {
            self.log
                .error("run event not committed", &failed(&key, &run, &fault));
        }
        key
    }

    // Takes up an agent started, or not, off the loop; the chat it is about.
    pub(super) async fn take_launched(&mut self, launched: Launched<A>) -> ChatKey {
        let (key, run) = (launched.key.clone(), launched.run.clone());
        let finished = self.finish_launch(launched).await;
        if let Some(fault) = self.unlogged(&key, Some(&run), finished).await {
            self.log
                .error("run start not handled", &failed(&key, &run, &fault));
        }
        key
    }
}

pub(super) fn is_unwritten(fault: &Fault) -> bool {
    [code!("chat.log_not_written"), code!("runs.not_saved")].contains(fault.code())
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;
    use weacon_protocol::{ChatAction, ChatCommand, RunEvent, RunState};
    use weacon_testing::messages;

    use super::super::agent::Said;
    use super::super::by_hand::{OnTurn, launched, on_ask, on_turn};
    use super::super::{Ask, update};
    use super::*;
    use crate::chats::test_support::{
        FakeAgent, Told, brief, chat, client, command, key, parts, prompt, pushed, request, run,
        someone, soon, text,
    };

    // Sends `command` as the engine's loop takes it, with its answer.
    async fn commanded(engine: &mut Engine<FakeAgent>, command: ChatCommand) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        engine
            .answer(Ask::Command(Box::new(command), someone(), answer))
            .await;
        answered.await.unwrap()
    }

    // The chat's last `count` events, in brief.
    async fn last(engine: &mut Engine<FakeAgent>, count: u32) -> Vec<String> {
        let log = &engine.open(&key()).await.unwrap().log;
        let window = log.window(u64::MAX, count).await.unwrap();
        window
            .into_iter()
            .map(|entry| brief(&update(&key(), entry)))
            .collect()
    }

    // A run on its turn whose agent was told the turn's prompt.
    async fn told() -> OnTurn {
        let mut on = on_turn().await;
        let told = soon(on.agent.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        on
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_whose_event_cant_be_logged_ends_failed_and_other_chats_go_on() {
        let mut on = told().await;
        on.engine.open(&key()).await.unwrap().log.jam();

        let from = FromRun {
            key: key(),
            run: run("run-1"),
            said: Said::Event(text("Hello")),
        };
        on.engine.take_said(from).await;

        assert_eq!(soon(on.agent.told.recv()).await, None);
        assert!(!on.engine.is_live(&key()));
        assert_eq!(pushed(&mut on.pushes, 1).await, ["events lost"]);
        assert_eq!(
            last(&mut on.engine, 2).await,
            [
                "5 receipt r-1 failed chat.log_not_written",
                "6 run-1 is Sleeping"
            ]
        );
        let record = on.engine.runs.of(&key()).unwrap();
        assert_eq!(
            (record.state, record.turn.as_ref()),
            (RunState::Sleeping, None)
        );
        let logged = serde_json::to_string(&*on.bench.records.lock().unwrap()).unwrap();
        let logged_once = messages(&on.bench.records);
        assert!(logged_once.contains(&"run ended unwritten"));
        assert!(!logged_once.contains(&"run event not committed"));
        assert!(logged.contains("chat.log_not_written") && logged.contains("c-1"));

        let elsewhere = command("r-1", "c-2", prompt("r-1", "Hi").action);
        commanded(&mut on.engine, elsewhere).await.unwrap();
        launched(&mut on.engine).await;
        let mut other = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(other.run_as.key.chat, chat("c-2"));
        assert_eq!(
            soon(other.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_command_whose_journal_save_fails_ends_the_run_and_the_next_prompt_gets_a_new_one() {
        let mut on = told().await;
        let path = on.engine.runs.jam();

        let failed = commanded(&mut on.engine, prompt("r-2", "More")).await;

        assert_eq!(failed.unwrap_err().code().as_str(), "runs.not_saved");
        assert_eq!(soon(on.agent.told.recv()).await, None);
        let ended = last(&mut on.engine, 1).await;
        assert_eq!(ended, ["6 receipt r-1 failed runs.not_saved"]);
        on.engine.runs.unjam(path);
        commanded(&mut on.engine, prompt("r-3", "Again"))
            .await
            .unwrap();
        launched(&mut on.engine).await;
        let next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_starting_when_its_chats_write_fails_is_let_go() {
        let (mut bench, mut engine) = parts();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        engine.open(&key()).await.unwrap().log.jam();

        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        let failed = commanded(&mut engine, interrupt).await;
        launched(&mut engine).await;

        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(soon(started.told.recv()).await, None);
        assert!(!engine.is_live(&key()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_whose_start_cant_be_logged_is_stopped_before_its_turn() {
        let (mut bench, mut engine) = parts();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        engine.open(&key()).await.unwrap().log.jam();

        let up = soon(engine.started.recv()).await.unwrap();
        engine.take_launched(up).await;

        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(soon(started.told.recv()).await, None);
        assert!(!engine.is_live(&key()));
        let record = engine.runs.of(&key()).unwrap();
        assert_eq!(record.state, RunState::Sleeping);
    }

    #[tokio::test(start_paused = true)]
    async fn a_launch_let_go_keeps_the_chat_busy_until_its_agent_is_up() {
        let (_bench, mut engine) = parts();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        engine.open(&key()).await.unwrap().log.jam();
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        commanded(&mut engine, interrupt).await.unwrap_err();

        let (holder, _pushes) = client(1);
        let hold = command("h-1", "c-1", ChatAction::Hold);
        let refused = engine.command_from(&key(), hold, &holder).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.busy");
        engine.command(&key(), prompt("r-3", "More")).await.unwrap();
        assert_eq!(engine.run_of(&key()), Some(run("run-1")));

        let up = soon(engine.started.recv()).await.unwrap();
        engine.take_launched(up).await;
        assert_eq!(engine.run_of(&key()), Some(run("run-2")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawal_that_cant_be_logged_ends_its_run() {
        let mut on = on_ask().await;
        on.engine.open(&key()).await.unwrap().log.jam();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };

        let from = FromRun {
            key: key(),
            run: run("run-1"),
            said: Said::Event(withdrawn),
        };
        on.engine.take_said(from).await;

        assert!(!on.engine.is_live(&key()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_running_state_cant_be_written_never_reaches_the_agent() {
        let mut on = told().await;
        let outcome = Outcome::Completed;
        let finished = RunEvent::Finished { outcome };
        on.engine
            .hear(&key(), &run("run-1"), Said::Event(finished))
            .await
            .unwrap();
        let queued = crate::runs::Queued {
            pick: None,
            request: request("r-2"),
            text: "More".to_owned(),
        };
        let agent = weacon_protocol::AgentKind::Claude;
        on.engine
            .runs
            .enqueue(&key(), agent, queued, 0)
            .await
            .unwrap();
        on.engine.open(&key()).await.unwrap().log.jam();

        let advanced = on.engine.advance(&key()).await;
        let failed = on.engine.checked(&key(), advanced).await;

        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_of_a_finished_runs_last_word_leaves_the_next_run_be() {
        let mut on = told().await;
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        on.engine.open(&key()).await.unwrap().log.jam();

        let finished = RunEvent::Finished {
            outcome: Outcome::Unknown,
        };
        let from = FromRun {
            key: key(),
            run: run("run-1"),
            said: Said::Event(finished),
        };
        on.engine.take_said(from).await;

        assert_eq!(on.engine.run_of(&key()), Some(run("run-2")));
        assert!(on.engine.is_launching(&key()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_whose_finished_turn_cant_be_logged_takes_no_queued_prompt() {
        let mut on = told().await;
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        on.engine.open(&key()).await.unwrap().log.jam();

        let outcome = Outcome::Completed;
        let from = FromRun {
            key: key(),
            run: run("run-1"),
            said: Said::Event(RunEvent::Finished { outcome }),
        };
        on.engine.take_said(from).await;

        assert_eq!(soon(on.agent.told.recv()).await, None);
        assert!(!on.engine.is_live(&key()));
    }
}
