//! How the engine starts a chat's run and keeps its agent's session.

use std::sync::Arc;

use tokio::sync::mpsc;
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, AgentSession, ChatTask, RepoId, RunId, RunState, code};

use super::agent::{Agent, drive};
use super::{ChatKey, Engine, about, failed};

/// Which agent to start, and as what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunAs {
    pub(crate) key: ChatKey,
    pub(crate) agent: AgentKind,
    pub(crate) run: RunId,
    pub(crate) session: Option<AgentSession>,
    /// The repository the chat's first prompt named, whose worktree it runs in.
    pub(crate) repo: Option<RepoId>,
    /// The task the chat's first prompt named, which the run's session starts from.
    pub(crate) task: Option<ChatTask>,
}

type Launcher<A> = Arc<dyn Fn(&RunAs) -> Result<A, Fault> + Send + Sync>;

/// What the engine takes from the machine; the daemon wires the real ones.
pub(crate) struct Host<A> {
    /// Starts an agent; may block, so it is called off the runtime.
    pub(crate) launch: Launcher<A>,
    /// Unix seconds.
    pub(crate) now: fn() -> u64,
}

/// A run's agent, or why it didn't start, back from starting off the engine's loop.
pub(super) struct Launched<A> {
    pub(super) key: ChatKey,
    pub(super) run: RunId,
    pub(super) agent: Result<A, Fault>,
}

pub(super) fn not_started() -> Fault {
    Fault::new(code!("agent.not_started"), "The agent couldn't start")
}

impl<A: Agent> Engine<A> {
    pub(super) fn is_launching(&self, key: &ChatKey) -> bool {
        self.open_chats
            .get(key)
            .is_some_and(|chat| chat.launching.is_some())
    }

    // Saves the session the chat's next run resumes.
    pub(super) async fn keep_session(
        &mut self,
        key: &ChatKey,
        session: AgentSession,
    ) -> Result<(), Fault> {
        self.runs
            .update(key, |record| record.session = Some(session))
            .await
    }

    // Starts the chat's agent as a new run, resuming its session when it has one. The agent
    // starts off the engine's loop, which `finish_launch` then takes up.
    pub(super) async fn launch(&mut self, key: &ChatKey) -> Result<(), Fault> {
        let kind = self.runs.of(key).ok_or_else(not_started)?.agent;
        // Read first, so a log that can't be opened leaves no run begun.
        let log = &self.open(key).await?.log;
        let (repo, task) = (log.repo().cloned(), log.task().cloned());
        let run = self.runs.begin(key, kind, self.now()).await?;
        let session = self.runs.of(key).and_then(|record| record.session.clone());
        let run_as = RunAs {
            key: key.clone(),
            agent: kind,
            run: run.clone(),
            session,
            repo,
            task,
        };
        self.open(key).await?.launching = Some(run.clone());
        let (start, starts) = (Arc::clone(&self.host.launch), self.starts.clone());
        let key = key.clone();
        tokio::spawn(async move {
            let agent = tokio::task::spawn_blocking(move || start(&run_as))
                .await
                .unwrap_or_else(|_| Err(not_started()));
            drop(starts.send(Launched { key, run, agent }));
        });
        Ok(())
    }

    // Takes up an agent started, or not, off the loop; the chat it is about.
    pub(super) async fn take_launched(&mut self, launched: Launched<A>) -> ChatKey {
        let (key, run) = (launched.key.clone(), launched.run.clone());
        if let Err(fault) = self.finish_launch(launched).await {
            self.log
                .error("run start not handled", &failed(&key, &run, &fault));
        }
        key
    }

    // Attaches a started agent and gives it the chat's next turn; one that didn't start fails
    // the queue.
    pub(super) async fn finish_launch(&mut self, launched: Launched<A>) -> Result<(), Fault> {
        let Launched { key, run, agent } = launched;
        self.open(&key).await?.launching = None;
        match agent {
            Ok(agent) => {
                self.attach(&key, &run, agent).await?;
                self.advance(&key).await?;
                // Started with no prompt, as after a restart, it waits for one.
                if self.turn(&key).is_none() {
                    self.set_state(&key, &run, RunState::Idle).await?;
                }
                Ok(())
            }
            Err(fault) => {
                self.log
                    .warn("run not started", &failed(&key, &run, &fault));
                // A run that stopped reading is let go of too, so the chat can close.
                self.open(&key).await?.orders = None;
                let ended = self.set_state(&key, &run, RunState::Ended).await;
                let failed = self.fail_queue(&key, &fault).await;
                ended.and(failed)
            }
        }
    }

    async fn attach(&mut self, key: &ChatKey, run: &RunId, agent: A) -> Result<(), Fault> {
        let (orders, ordered) = mpsc::unbounded_channel();
        let says = self.says.clone();
        tokio::spawn(drive(agent, key.clone(), run.clone(), ordered, says));
        self.open(key).await?.orders = Some(orders);
        self.log.info("run started", &about(key, run));
        self.tell_state(key, run, RunState::Starting).await
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Outcome, RunEvent};
    use weacon_testing::messages;

    use super::super::agent::Said;
    use super::super::by_hand::{hear, launched, on_turn, until_deaf};
    use crate::chats::test_support::{key, parts, prompt, run, session, soon};

    #[tokio::test(start_paused = true)]
    async fn a_session_the_journal_couldnt_save_is_lost_and_the_next_run_starts_a_new_one() {
        let mut on = on_turn().await;
        let path = on.engine.runs.jam();
        let started = RunEvent::Started { session: session() };
        let lost = hear(&mut on.engine, started).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "runs.not_saved");
        on.engine.runs.unjam(path);

        // The agent exits and nobody watches.
        drop((on.agent, on.pushes));
        let exited = Said::Exited(None);
        on.engine.hear(&key(), &run("run-1"), exited).await.unwrap();
        on.engine.close_if_unused(&key());
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();

        let next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
        assert_eq!(next.run_as.session, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_whose_start_cant_be_logged_is_logged_once_as_an_error_and_keeps_its_agent() {
        let (bench, mut engine) = parts();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        engine.open(&key()).await.unwrap().log.jam();

        let launched = soon(engine.started.recv()).await.unwrap();
        assert_eq!(engine.take_launched(launched).await, key());

        assert!(engine.is_live(&key()));
        let logged = messages(&bench.records);
        let errors = logged.iter().filter(|msg| **msg == "run start not handled");
        assert_eq!(errors.count(), 1, "{logged:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_whose_new_run_cant_start_lets_go_of_the_old_one_and_closes() {
        let mut on = on_turn().await;
        drop((on.agent, on.pushes));
        until_deaf(&on.engine).await;
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
        launched(&mut on.engine).await;
        on.engine.close_if_unused(&key());

        assert!(!on.engine.open_chats.contains_key(&key()));
    }
}
