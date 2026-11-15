//! A write that fails, to a chat's log or to the runs journal, ends that chat's run, so the
//! run on disk never disagrees with what happens. Other chats go on.

use weacon_observability::Fault;
use weacon_protocol::{Outcome, code};

use super::agent::{Agent, Said};
use super::launch::Launched;
use super::{ChatKey, Engine, about, failed};

impl<A: Agent> Engine<A> {
    // Passes `result` on, ending the chat's run first when it is a failed write.
    pub(super) async fn checked<T>(
        &mut self,
        key: &ChatKey,
        result: Result<T, Fault>,
    ) -> Result<T, Fault> {
        if let Err(fault) = &result
            && is_unwritten(fault)
        {
            self.end_run(key, fault).await;
        }
        result
    }

    // Tells the chat's agent to stop, and ends its run failed where that can still be written;
    // a turn whose end can't be is ended by the chat's next `advance`.
    async fn end_run(&mut self, key: &ChatKey, fault: &Fault) {
        let Some(run) = self.run_of(key) else {
            return;
        };
        let Some(chat) = self.open_chats.get_mut(key) else {
            return;
        };
        let stopped = chat.orders.take().is_some();
        let starting = chat.launching.take().is_some();
        if !stopped && !starting {
            return;
        }
        chat.pending_interrupts.clear();
        let [workspace, chat, run_id] = about(key, &run);
        let code_field = ("code", fault.code().as_str().into());
        self.log.warn(
            "run ended unwritten",
            &[workspace, chat, run_id, code_field],
        );
        let code = fault.code().clone();
        drop(self.end_turn(key, &run, Outcome::Failed { code }).await);
    }

    // Takes up what a run said; the chat it is about.
    pub(super) async fn take_said(&mut self, said: Said) -> ChatKey {
        let (key, run) = (said.of.key, said.of.run);
        let heard = self.heard(&key, &run, said.heard).await;
        if let Err(fault) = self.checked(&key, heard).await {
            self.log
                .error("run event not committed", &failed(&key, &run, &fault));
        }
        key
    }

    // Takes up an agent started, or not, off the loop; the chat it is about.
    pub(super) async fn take_launched(&mut self, launched: Launched<A>) -> ChatKey {
        let (key, run) = (launched.key.clone(), launched.run.clone());
        let finished = self.finish_launch(launched).await;
        if let Err(fault) = self.checked(&key, finished).await {
            self.log
                .error("run start not handled", &failed(&key, &run, &fault));
        }
        key
    }
}

fn is_unwritten(fault: &Fault) -> bool {
    [code!("chat.log_not_written"), code!("runs.not_saved")].contains(fault.code())
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;
    use weacon_protocol::{ChatAction, ChatCommand, RunState};
    use weacon_testing::messages;

    use super::super::agent::{Heard, RunOf};
    use super::super::by_hand::{OnTurn, launched, on_turn};
    use super::super::{Ask, update};
    use super::*;
    use crate::chats::test_support::{
        FakeAgent, Told, brief, chat, command, key, parts, prompt, pushed, run, someone, soon, text,
    };

    // Sends `command` as the engine's loop takes it, with its answer.
    async fn commanded(engine: &mut Engine<FakeAgent>, command: ChatCommand) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        engine
            .answer(Ask::Command(command, someone(), answer))
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

        let said = Said {
            of: RunOf {
                key: key(),
                run: run("run-1"),
            },
            heard: Heard::Event(text("Hello")),
        };
        on.engine.take_said(said).await;

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
        assert!(messages(&on.bench.records).contains(&"run ended unwritten"));
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
}
