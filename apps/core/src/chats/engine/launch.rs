//! How the engine starts a chat's run and keeps its agent's session.

use std::sync::Arc;

use tokio::sync::mpsc;
use weacon_observability::Fault;
use weacon_protocol::{AgentSession, RunId, RunState, code};

use super::agent::{Agent, RunOf, drive};
use super::{ChatKey, Engine, RunAs, about, failed};

fn not_started() -> Fault {
    Fault::new(code!("agent.not_started"), "The agent couldn't start")
}

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
        self.save_unsaved(key).await?;
        let kind = self.runs.of(key).ok_or_else(not_started)?.agent;
        let run = self.runs.begin(key, kind, self.now()).await?;
        let session = self.runs.of(key).and_then(|record| record.session.clone());
        let run_as = RunAs {
            chat: key.clone(),
            agent: kind,
            run: run.clone(),
            session,
        };
        match self.launch_agent(run_as).await {
            Ok(agent) => self.attach(key, &run, agent).await?,
            Err(fault) => {
                self.log.warn("run not started", &failed(key, &run, &fault));
                // A run that stopped reading is let go of too, so the chat can close.
                self.open(key).await?.orders = None;
                self.set_state(key, &run, RunState::Ended).await?;
                return Err(fault);
            }
        }
        Ok(run)
    }

    // Starting an agent may block, so it runs off the runtime.
    async fn launch_agent(&self, run_as: RunAs) -> Result<A, Fault> {
        let start = Arc::clone(&self.host.launch);
        tokio::task::spawn_blocking(move || start(&run_as))
            .await
            .unwrap_or_else(|_| Err(not_started()))
    }

    // Hands the started agent to a task of its own, which the chat then orders.
    async fn attach(&mut self, key: &ChatKey, run: &RunId, agent: A) -> Result<(), Fault> {
        let (orders, ordered) = mpsc::unbounded_channel();
        let of = RunOf {
            chat: key.clone(),
            run: run.clone(),
        };
        tokio::spawn(drive(agent, of, ordered, self.says.clone()));
        self.open(key).await?.orders = Some(orders);
        self.log.info("run started", &about(key, run));
        // The agent is up: a status that can't be committed doesn't stop its turn.
        if let Err(fault) = self.set_state(key, run, RunState::Starting).await {
            self.log
                .warn("run status not committed", &failed(key, run, &fault));
        }
        Ok(())
    }

    pub(super) fn run_of(&self, key: &ChatKey) -> Option<RunId> {
        self.runs.of(key)?.run.clone()
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Outcome, RunEvent};

    use super::super::agent::Heard;
    use super::super::by_hand::{hear, on_turn};
    use crate::chats::test_support::{key, prompt, run, session, soon};

    #[tokio::test(start_paused = true)]
    async fn a_session_the_journal_couldnt_save_is_resumed_by_the_next_run() {
        let mut on = on_turn().await;
        let path = on.engine.runs.jam();
        let started = RunEvent::Started { session: session() };
        let lost = hear(&mut on.engine, started).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "runs.not_saved");
        on.engine.runs.unjam(path);

        // The agent exits and nobody watches: the chat stays open for its session.
        drop((on.agent, on.pushes));
        let exited = Heard::Exited(None);
        on.engine
            .heard(&key(), &run("run-1"), exited)
            .await
            .unwrap();
        on.engine.settle(&key());
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();

        let next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
        assert_eq!(next.run_as.session, Some(session()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_whose_new_run_cant_start_lets_go_of_the_old_one_and_closes() {
        let mut on = on_turn().await;
        drop((on.agent, on.pushes));
        let mut tries = 0;
        while on.engine.takes_orders(&key()) && tries < 1000 {
            tries += 1;
            tokio::task::yield_now().await;
        }
        on.bench
            .broken
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        on.engine.settle(&key());

        assert!(!on.engine.open_chats.contains_key(&key()));
    }
}
