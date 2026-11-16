//! How the engine lets an idle agent go, and picks its chats up again after a restart.

use weacon_observability::{Fault, Field};
use weacon_protocol::{RunState, code};

use super::agent::Agent;
use super::{Chat, ChatKey, Engine, about, chat_about};

/// How long, in seconds, an agent waits for its chat's next prompt before it is stopped.
pub(super) const IDLE: u64 = 30 * 60;

impl<A: Agent> Engine<A> {
    // Stops each agent idle for `IDLE`, never on a turn, while a person is asked something or
    // while a write is owed. Its session stays, so the chat's next prompt resumes it.
    pub(super) async fn sleep_idle(&mut self) {
        // The backstop for a connection whose end wasn't reported.
        self.close_unused().await;
        let now = self.now();
        let idle: Vec<ChatKey> = self
            .open_chats
            .iter()
            .filter(|(key, chat)| self.dozes(key, chat, now))
            .map(|(key, _)| key.clone())
            .collect();
        for key in idle {
            let slept = self.sleep(&key).await;
            if let Some(fault) = self.unlogged(&key, None, slept).await {
                self.log.warn("run not stopped", &chat_fields(&key, &fault));
            }
            self.close_if_unused(&key);
        }
    }

    fn dozes(&self, key: &ChatKey, chat: &Chat, now: u64) -> bool {
        let Some(record) = self.runs.of(key) else {
            return false;
        };
        chat.orders.is_some()
            && chat.launching.is_none()
            && record.turn.is_none()
            && chat.log.runs().waiting().is_empty()
            && chat.pending_end.is_none()
            && record.active_at.saturating_add(IDLE) <= now
    }

    pub(super) async fn rest(&mut self, key: &ChatKey) -> Result<(), Fault> {
        match self.runs.of(key).map(|record| record.state) {
            Some(RunState::Starting | RunState::Running | RunState::Waiting) => Err(Fault::new(
                code!("worktree.busy"),
                "Stop the chat's agent before removing its worktree",
            )),
            Some(RunState::Idle) => {
                self.sleep(key).await?;
                self.close_if_unused(key);
                Ok(())
            }
            Some(RunState::Sleeping | RunState::Ended | RunState::Newer) | None => Ok(()),
        }
    }

    // Dropping its orders stops the agent; its run sleeps.
    async fn sleep(&mut self, key: &ChatKey) -> Result<(), Fault> {
        let Some(run) = self.run_of(key) else {
            return Ok(());
        };
        self.open(key).await?.orders = None;
        self.log.info("run stopped idle", &about(key, &run));
        self.set_state(key, &run, RunState::Sleeping).await
    }

    // Every `false` lets the chats with prompts queued go on: the disk may have filled and
    // cleared before the engine looked.
    pub(super) async fn set_disk_full(&mut self, full: bool) {
        match (std::mem::replace(&mut self.disk_full, full), full) {
            (false, true) => self.log.warn("turns paused", &[]),
            (true, false) => self.log.info("turns resumed", &[]),
            _ => {}
        }
        if !full {
            self.go_on().await;
        }
    }

    // Lets every chat with prompts queued go on, where nothing else holds its turns back.
    pub(super) async fn go_on(&mut self) {
        let queued: Vec<ChatKey> = self
            .runs
            .records()
            .filter(|record| !record.queue.is_empty())
            .map(|record| record.key.clone())
            .collect();
        for key in queued {
            let advanced = self.advance(&key).await;
            if let Some(fault) = self.unlogged(&key, None, advanced).await {
                self.log
                    .warn("chat not picked up", &chat_fields(&key, &fault));
            }
            self.close_if_unused(&key);
        }
    }

    // After a restart: a turn cut off ends `unknown` and its run sleeps, queued prompts get a new
    // run, and an idle run active within `IDLE` starts again to resume its session; the others
    // sleep.
    pub(super) async fn reconcile(&mut self) {
        let kept = self.runs.kept();
        if kept > 0 {
            let kept = u64::try_from(kept).unwrap_or(u64::MAX);
            self.log
                .warn("runs records kept unread", &[("count", kept.into())]);
        }
        let now = self.now();
        let awake: Vec<(ChatKey, bool)> = self
            .runs
            .records()
            .filter(|record| {
                !record.queue.is_empty()
                    || !matches!(record.state, RunState::Sleeping | RunState::Ended)
            })
            .map(|record| {
                (
                    record.key.clone(),
                    record.active_at.saturating_add(IDLE) > now,
                )
            })
            .collect();
        for (key, recent) in awake {
            let picked = self.pick_up(&key, recent).await;
            if let Some(fault) = self.unlogged(&key, None, picked).await {
                self.log
                    .warn("chat not picked up", &chat_fields(&key, &fault));
            }
            self.close_if_unused(&key);
        }
    }

    async fn pick_up(&mut self, key: &ChatKey, recent: bool) -> Result<(), Fault> {
        self.advance(key).await?;
        let Some(record) = self.runs.of(key) else {
            return Ok(());
        };
        if self.is_launching(key) || matches!(record.state, RunState::Sleeping | RunState::Ended) {
            return Ok(());
        }
        if recent {
            return self.launch(key).await;
        }
        let Some(run) = self.run_of(key) else {
            return Ok(());
        };
        self.set_state(key, &run, RunState::Sleeping).await
    }
}

pub(super) fn chat_fields(key: &ChatKey, fault: &Fault) -> [(&'static str, Field); 3] {
    let [workspace, chat] = chat_about(key);
    [workspace, chat, ("err", Field::error(fault))]
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AgentKind, Outcome, RunEvent};
    use weacon_testing::messages;

    use super::super::by_hand::{OnTurn, hear, launched, on_ask, on_turn};
    use super::*;
    use crate::chats::test_support::{
        FakeAgent, Told, chat, key, parts, prompt, run, session, soon,
    };
    use crate::runs::Queued;

    #[tokio::test(start_paused = true)]
    async fn a_disk_full_and_clear_again_before_the_engine_looks_still_starts_the_queued_turn() {
        let (bench, mut engine) = parts();
        bench.chats.disk_full(true);
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        bench.chats.disk_full(false);

        engine.set_disk_full(false).await;

        launched(&mut engine).await;
        assert!(!messages(&bench.records).contains(&"turns resumed"));
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_change_of_the_disk_is_logged() {
        let (bench, mut engine) = parts();

        for full in [true, true, false, false, true] {
            engine.set_disk_full(full).await;
        }

        let turns: Vec<_> = messages(&bench.records)
            .into_iter()
            .filter(|message| message.starts_with("turns "))
            .collect();
        assert_eq!(turns, ["turns paused", "turns resumed", "turns paused"]);
    }

    // Makes chat `c-1`'s last turn start or end `ago` seconds before now.
    async fn active_ago(engine: &mut Engine<FakeAgent>, ago: u64) {
        let at = engine.now() - ago;
        let chat = key();
        let update = engine.runs.update(&chat, |record| record.active_at = at);
        update.await.unwrap();
    }

    fn state(engine: &Engine<FakeAgent>) -> RunState {
        engine.runs.of(&key()).unwrap().state
    }

    // A run that said its session and finished its turn, so it waits for the next one.
    async fn between_turns() -> OnTurn {
        let mut on = on_turn().await;
        let told = soon(on.agent.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        let started = RunEvent::Started { session: session() };
        hear(&mut on.engine, started).await.unwrap();
        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        on
    }

    #[test]
    fn idle_is_half_an_hour() {
        assert_eq!(IDLE, 1800);
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_idle_for_half_an_hour_stops_and_the_next_prompt_resumes_its_session() {
        let mut on = between_turns().await;
        active_ago(&mut on.engine, IDLE - 1).await;
        on.engine.sleep_idle().await;
        assert!(on.engine.is_live(&key()));

        active_ago(&mut on.engine, IDLE).await;
        on.engine.sleep_idle().await;
        assert!(!on.engine.is_live(&key()));
        assert_eq!(soon(on.agent.told.recv()).await, None);
        assert_eq!(state(&on.engine), RunState::Sleeping);

        let next = prompt("r-2", "More");
        on.engine.command(&key(), next).await.unwrap();
        launched(&mut on.engine).await;
        let mut next = soon(on.bench.started.recv()).await.unwrap();
        let run_as = (next.run_as.run, next.run_as.session);
        assert_eq!(run_as, (run("run-2"), Some(session())));
        let told = soon(next.told.recv()).await;
        assert_eq!(told, Some(Told::Send("More".to_owned())));
    }

    #[tokio::test(start_paused = true)]
    async fn a_rest_stops_an_agent_between_turns_and_the_next_prompt_starts_it_again() {
        let mut on = between_turns().await;

        on.engine.rest(&key()).await.unwrap();

        assert!(!on.engine.is_live(&key()));
        assert_eq!(state(&on.engine), RunState::Sleeping);
        on.engine.rest(&key()).await.unwrap();
        let next = prompt("r-2", "More");
        on.engine.command(&key(), next).await.unwrap();
        launched(&mut on.engine).await;
        let next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.run, run("run-2"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_rest_is_refused_on_a_turn_and_nothing_to_a_chat_without_a_run() {
        for mut on in [on_turn().await, on_ask().await] {
            let refused = on.engine.rest(&key()).await.unwrap_err();

            assert_eq!(refused.code().as_str(), "worktree.busy");
            assert!(on.engine.is_live(&key()));
        }
        let (_bench, mut engine) = parts();
        engine.rest(&key()).await.unwrap();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        let starting = engine.rest(&key()).await.unwrap_err();
        assert_eq!(starting.code().as_str(), "worktree.busy");
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_on_its_turn_or_asking_a_person_is_never_stopped() {
        for mut on in [on_turn().await, on_ask().await] {
            active_ago(&mut on.engine, IDLE * 2).await;

            on.engine.sleep_idle().await;

            assert!(on.engine.is_live(&key()));
            assert_ne!(state(&on.engine), RunState::Sleeping);
        }
    }

    // A record for chat `id` whose run was last in `state`, `ago` seconds before now.
    async fn left(engine: &mut Engine<FakeAgent>, id: &str, state: RunState, ago: u64) -> ChatKey {
        let chat = ChatKey {
            chat: chat(id),
            ..key()
        };
        let at = engine.now() - ago;
        engine
            .runs
            .begin(&chat, AgentKind::Claude, at)
            .await
            .unwrap();
        let update = engine.runs.update(&chat, |record| record.state = state);
        update.await.unwrap();
        chat
    }

    #[tokio::test(start_paused = true)]
    async fn after_a_restart_a_cut_off_turn_ends_and_queued_and_recent_runs_start_again() {
        let mut on = on_turn().await;
        let more = prompt("r-2", "More");
        on.engine.command(&key(), more).await.unwrap();
        let recent = left(&mut on.engine, "c-2", RunState::Idle, 60).await;
        let old = left(&mut on.engine, "c-3", RunState::Idle, IDLE).await;
        let asleep = left(&mut on.engine, "c-4", RunState::Sleeping, 60).await;
        let waits = left(&mut on.engine, "c-5", RunState::Sleeping, IDLE * 2).await;
        let queued = Queued {
            pick: None,
            request: prompt("r-9", "Later").request,
            text: "Later".to_owned(),
        };
        let enqueue = on.engine.runs.enqueue(&waits, AgentKind::Claude, queued, 1);
        enqueue.await.unwrap();
        // As after a restart: nothing of any chat or its run is in memory.
        on.engine.open_chats.clear();

        on.engine.reconcile().await;
        for _ in 0..3 {
            launched(&mut on.engine).await;
        }
        assert!(!messages(&on.bench.records).contains(&"runs records kept unread"));

        let first = prompt("r-1", "Hi").request;
        let ended = on.engine.open(&key()).await.unwrap().log.has_ended(&first);
        assert!(ended);
        let mut started = Vec::new();
        while let Ok(next) = on.bench.started.try_recv() {
            started.push(next);
        }
        started.sort_by_key(|next| next.run_as.key.chat.as_str().to_owned());
        let keys: Vec<_> = started.iter().map(|next| next.run_as.key.clone()).collect();
        assert_eq!(keys, [key(), recent.clone(), waits]);
        let told = soon(started[0].told.recv()).await;
        assert_eq!(told, Some(Told::Send("More".to_owned())));
        let states = [&recent, &old, &asleep].map(|chat| on.engine.runs.of(chat).unwrap().state);
        assert_eq!(
            states,
            [RunState::Idle, RunState::Sleeping, RunState::Sleeping]
        );
        assert!(!on.engine.open_chats.contains_key(&asleep));
    }
}
