//! How the engine starts a chat's run and keeps its agent's session.

use std::sync::Arc;

use tokio::sync::mpsc;
use weacon_observability::{Fault, Field};
use weacon_protocol::{AgentSession, RunId, RunState, code};

use super::agent::{Agent, RunOf, drive};
use super::{ChatKey, Engine, Start};

impl<A: Agent> Engine<A> {
    // Saves the session the chat's next run resumes; one that can't be saved yet is kept, and
    // saved before that run starts.
    pub(super) async fn keep_session(
        &mut self,
        key: &ChatKey,
        session: AgentSession,
    ) -> Result<(), Fault> {
        let given = session.clone();
        let kept = self
            .runs
            .update(key, |record| record.session = Some(given))
            .await;
        self.open(key).await?.pending_session = kept.is_err().then_some(session);
        kept
    }

    async fn save_unsaved(&mut self, key: &ChatKey) -> Result<(), Fault> {
        match self.open(key).await?.pending_session.take() {
            Some(session) => self.keep_session(key, session).await,
            None => Ok(()),
        }
    }

    // Starts the chat's agent as a new run, resuming its session when it has one.
    pub(super) async fn launch(&mut self, key: &ChatKey) -> Result<RunId, Fault> {
        let not_started = || Fault::new(code!("agent.not_started"), "The agent couldn't start");
        self.save_unsaved(key).await?;
        let agent = self.runs.of(key).ok_or_else(not_started)?.agent;
        let now = (self.host.now)();
        let run = self.runs.begin(key, agent, now).await?;
        let record = self.runs.of(key);
        let run_as = Start {
            chat: key.clone(),
            agent,
            run: run.clone(),
            session: record.and_then(|record| record.session.clone()),
        };
        let start = Arc::clone(&self.host.start);
        let agent = tokio::task::spawn_blocking(move || start(&run_as))
            .await
            .unwrap_or_else(|_| Err(not_started()));
        let agent = match agent {
            Ok(agent) => agent,
            Err(fault) => {
                let fields = [
                    ("run_id", run.as_str().into()),
                    ("err", Field::error(&fault)),
                ];
                self.log.warn("run not started", &fields);
                // A run that stopped reading is let go of too, so the chat can close.
                self.open(key).await?.orders = None;
                self.set_state(key, &run, RunState::Ended).await?;
                return Err(fault);
            }
        };
        let (orders, ordered) = mpsc::unbounded_channel();
        let of = RunOf {
            chat: key.clone(),
            run: run.clone(),
        };
        tokio::spawn(drive(agent, of, ordered, self.reporter.clone()));
        self.open(key).await?.orders = Some(orders);
        self.log
            .info("run started", &[("run_id", run.as_str().into())]);
        // The agent is up: a status that can't be committed doesn't stop its turn.
        if let Err(fault) = self.set_state(key, &run, RunState::Starting).await {
            let fields = [
                ("run_id", run.as_str().into()),
                ("err", Field::error(&fault)),
            ];
            self.log.warn("run status not committed", &fields);
        }
        Ok(run)
    }

    pub(super) fn run_of(&self, key: &ChatKey) -> Option<RunId> {
        self.runs.of(key)?.run.clone()
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Outcome, RunEvent};

    use super::super::agent::Heard;
    use super::super::by_hand::{hear, on_turn, session};
    use crate::chats::test_support::{key, prompt, run, soon};

    #[tokio::test(start_paused = true)]
    async fn a_session_the_journal_couldnt_save_is_resumed_by_the_next_run() {
        let (mut bench, mut engine, agent, pushes) = on_turn().await;
        let path = engine.runs.jam();
        let started = RunEvent::Started { session: session() };
        let lost = hear(&mut engine, started).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "runs.not_saved");
        engine.runs.unjam(path);

        // The agent exits and nobody watches: the chat stays open for its session.
        drop((agent, pushes));
        let exited = Heard::Exited(None);
        engine.heard(&key(), &run("run-1"), exited).await.unwrap();
        engine.settle(&key());
        engine.command(&key(), prompt("r-2", "More")).await.unwrap();

        let next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(next.start.run, run("run-2"));
        assert_eq!(next.start.session, Some(session()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_whose_new_run_cant_start_lets_go_of_the_old_one_and_closes() {
        let (bench, mut engine, agent, pushes) = on_turn().await;
        drop((agent, pushes));
        let mut tries = 0;
        while engine.takes_orders(&key()) && tries < 1000 {
            tries += 1;
            tokio::task::yield_now().await;
        }
        bench
            .broken
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        engine.command(&key(), prompt("r-2", "More")).await.unwrap();
        engine.settle(&key());

        assert!(!engine.chats.contains_key(&key()));
    }
}
