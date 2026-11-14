//! What the engine does with a run: start it, give it its next turn, and commit what it says.

use std::sync::Arc;

use tokio::sync::mpsc;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentSession, ChatEntry, ChatEvent, Message, Outcome, PermissionAnswer, RunEvent, RunId,
    RunState, RunStatus, RunUpdate, code,
};

use super::cut::cut;
use super::{Engine, Key, Start};
use crate::runs::Queued;
use crate::runs::task::{Agent, Heard, Order, RunOf, drive};

impl<A: Agent> Engine<A> {
    pub(super) async fn heard(
        &mut self,
        key: &Key,
        run: &RunId,
        heard: Heard,
    ) -> Result<(), Fault> {
        if !self.is_live(key) || self.run_of(key).as_ref() != Some(run) {
            return Ok(());
        }
        match heard {
            Heard::Exited(fault) => {
                if let Some(fault) = fault {
                    let fields = [
                        ("run_id", run.as_str().into()),
                        ("err", Field::error(&fault)),
                    ];
                    self.log.warn("run output not read", &fields);
                }
                self.exited(key, run).await
            }
            Heard::Event(event @ RunEvent::TextDelta { .. }) => {
                let update = RunUpdate {
                    run: run.clone(),
                    event,
                };
                self.publish(key, &Message::Run(update));
                Ok(())
            }
            Heard::Event(event) => self.said(key, run, event).await,
        }
    }

    // Commits what the run said and acts on it.
    async fn said(&mut self, key: &Key, run: &RunId, event: RunEvent) -> Result<(), Fault> {
        let committed = self.commit_said(key, run, &event).await;
        match event {
            // Kept whether or not the event was committed: the chat's next run resumes it.
            RunEvent::Started { session } => {
                let kept = self.keep_session(key, session).await;
                committed.and(kept)
            }
            RunEvent::PermissionAsked { prompt, .. } => {
                self.asked(key, run, prompt, committed).await
            }
            // Tried again after a failed write, which reopens the log, so it never shows the
            // prompt still waiting.
            RunEvent::PermissionWithdrawn { prompt, .. } => {
                // An answer still on its way to a withdrawn prompt is never given, even if the
                // withdrawal can't be written.
                let answers = &mut self.chat(key).await?.answers;
                answers.retain(|answered| answered.answer.prompt != prompt);
                if committed.is_err() {
                    self.commit_said(key, run, &event).await?;
                }
                let log = &self.chat(key).await?.log;
                let asks = log.runs().waiting.iter().any(|waits| waits.run == *run);
                if !asks && self.turn(key).is_some() {
                    self.state(key, run, RunState::Running).await?;
                }
                Ok(())
            }
            // An agent that leaves its turn unknown is exiting; nothing more goes to it.
            RunEvent::Finished {
                outcome: Outcome::Unknown,
            } => {
                let exited = self.exited(key, run).await;
                committed.and(exited)
            }
            // The turn is over whether or not its last event could be committed.
            RunEvent::Finished { outcome } => {
                let ended = self.finished(key, run, outcome, RunState::Idle).await;
                let advanced = self.advance(key).await;
                committed.and(ended).and(advanced)
            }
            RunEvent::TextDelta { .. }
            | RunEvent::Text { .. }
            | RunEvent::ToolCall { .. }
            | RunEvent::ToolDone { .. }
            | RunEvent::PermissionAnswered { .. }
            | RunEvent::Newer => committed.map(drop),
        }
    }

    // A prompt a person can't be shown in full is refused for them; it never waits unseen.
    async fn asked(
        &mut self,
        key: &Key,
        run: &RunId,
        prompt: u32,
        committed: Result<(ChatEntry, bool), Fault>,
    ) -> Result<(), Fault> {
        if let Ok((_, true)) = &committed {
            return self.state(key, run, RunState::Waiting).await;
        }
        let answer = PermissionAnswer {
            run: run.clone(),
            prompt,
            allow: false,
        };
        self.order(key, Order::Answer(answer));
        self.log.warn(
            "permission prompt refused unseen",
            &[("run_id", run.as_str().into())],
        );
        // Withdrawn wherever it reached the log, even by a write reported failed.
        let logged = self.chat(key).await?.log.runs().waits(run, prompt);
        if committed.is_ok() || logged {
            let event = RunEvent::PermissionWithdrawn {
                run: run.clone(),
                prompt,
            };
            self.commit_said(key, run, &event).await?;
        }
        committed.map(drop)
    }

    // Commits a run's event, cut down when it is too large for one frame; true when whole.
    async fn commit_said(
        &mut self,
        key: &Key,
        run: &RunId,
        event: &RunEvent,
    ) -> Result<(ChatEntry, bool), Fault> {
        let entry = |event: RunEvent| ChatEvent::Run {
            update: RunUpdate {
                run: run.clone(),
                event,
            },
        };
        match self.commit(key, entry(event.clone())).await {
            Err(fault) if fault.code() == &code!("chat.event_too_large") => {
                let cut = self.commit(key, entry(cut(event))).await?;
                Ok((cut, false))
            }
            committed => Ok((committed?, true)),
        }
    }

    // Ends the turn, if one is running, with `outcome`, and leaves the run in `state`.
    async fn finished(
        &mut self,
        key: &Key,
        run: &RunId,
        outcome: Outcome,
        state: RunState,
    ) -> Result<(), Fault> {
        let (turn, now) = (self.turn(key), (self.host.now)());
        let chat = self.chat(key).await?;
        chat.answers.clear();
        // What the run reported first wins over a later end, such as its exit.
        let outcome = match chat.unended.take() {
            Some((ended, reported)) if turn.as_ref() == Some(&ended) => reported,
            _ => outcome,
        };
        // The turn stays in the journal until its receipt is in the log; `advance` retries it.
        if let Some(request) = turn
            && let Err(fault) = self.receipt(key, &request, outcome.clone()).await
        {
            if let Some(chat) = self.chats.get_mut(key) {
                chat.unended = Some((request, outcome));
            }
            return Err(fault);
        }
        // The state goes in the same save, so no record is left running with no turn.
        self.runs
            .update(&key.0, &key.1, |record| {
                record.turn = None;
                record.state = state;
                record.active_at = now;
            })
            .await?;
        self.state(key, run, state).await
    }

    // The agent is gone: its turn ends unknown, and what is queued goes to the chat's next run.
    async fn exited(&mut self, key: &Key, run: &RunId) -> Result<(), Fault> {
        self.chat(key).await?.orders = None;
        self.finished(key, run, Outcome::Unknown, RunState::Sleeping)
            .await?;
        self.advance(key).await
    }

    // Starts the next queued prompt's turn, in a new run when the chat's agent isn't running.
    pub(super) async fn advance(&mut self, key: &Key) -> Result<(), Fault> {
        self.save_ended(key).await?;
        let Some(record) = self.runs.of(&key.0, &key.1) else {
            return Ok(());
        };
        // A turn whose agent is gone is over, even if the journal couldn't be told.
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
    async fn give_turn(&mut self, key: &Key, run: &RunId, next: Queued) -> Result<(), Fault> {
        let now = (self.host.now)();
        self.runs
            .update(&key.0, &key.1, |record| {
                record.queue.retain(|queued| queued.request != next.request);
                record.turn = Some(next.request.clone());
                record.active_at = now;
            })
            .await?;
        if !self.order(key, Order::Send(next.text.clone())) {
            // It stopped reading while the journal was saved: the prompt waits for the next run.
            return self
                .runs
                .update(&key.0, &key.1, |record| {
                    record.turn = None;
                    record.queue.insert(0, next);
                })
                .await;
        }
        self.state(key, run, RunState::Running).await
    }

    // Saves the ends a failed write or an exit left unsaved: the turn's, and those of queued
    // prompts, which then never run.
    pub(super) async fn save_ended(&mut self, key: &Key) -> Result<(), Fault> {
        let Some(record) = self.runs.of(&key.0, &key.1) else {
            return Ok(());
        };
        let (turn, queue) = (record.turn.clone(), record.queue.clone());
        let live = self.is_live(key);
        let chat = self.chat(key).await?;
        // A turn whose run is gone, as after a restart, is over too, and so is that run.
        let turn_ended =
            turn.is_some_and(|turn| !live || chat.unended.is_some() || chat.log.has_ended(&turn));
        let working = |status: &RunStatus| {
            matches!(
                status.state,
                RunState::Starting | RunState::Running | RunState::Waiting
            )
        };
        let cut_off = !live && chat.log.runs().runs.iter().any(working);
        let ended: Vec<_> = queue
            .into_iter()
            .map(|queued| queued.request)
            .filter(|request| chat.log.has_ended(request))
            .collect();
        if !ended.is_empty() {
            self.runs
                .update(&key.0, &key.1, |record| {
                    record
                        .queue
                        .retain(|queued| !ended.contains(&queued.request));
                })
                .await?;
        }
        let state = if live {
            RunState::Idle
        } else {
            RunState::Sleeping
        };
        match self.run_of(key) {
            Some(run) if turn_ended || cut_off => {
                self.finished(key, &run, Outcome::Unknown, state).await
            }
            _ => Ok(()),
        }
    }

    // Nothing can run them: each queued prompt fails rather than waits for ever.
    async fn fail_queue(&mut self, key: &Key, fault: &Fault) -> Result<(), Fault> {
        let record = self.runs.of(&key.0, &key.1);
        let queue = record
            .map(|record| record.queue.clone())
            .unwrap_or_default();
        // Each one leaves the queue only once its receipt is in the log.
        for queued in queue {
            let code = fault.code().clone();
            self.receipt(key, &queued.request, Outcome::Failed { code })
                .await?;
        }
        self.save_ended(key).await
    }

    // Saves the session the chat's next run resumes; one that can't be saved yet is kept, and
    // saved before that run starts.
    async fn keep_session(&mut self, key: &Key, session: AgentSession) -> Result<(), Fault> {
        let given = session.clone();
        let kept = self
            .runs
            .update(&key.0, &key.1, |record| record.session = Some(given))
            .await;
        self.chat(key).await?.unsaved = kept.is_err().then_some(session);
        kept
    }

    async fn save_unsaved(&mut self, key: &Key) -> Result<(), Fault> {
        match self.chat(key).await?.unsaved.take() {
            Some(session) => self.keep_session(key, session).await,
            None => Ok(()),
        }
    }

    // Starts the chat's agent as a new run, resuming its session when it has one.
    async fn launch(&mut self, key: &Key) -> Result<RunId, Fault> {
        let not_started = || Fault::new(code!("agent.not_started"), "The agent couldn't start");
        self.save_unsaved(key).await?;
        let agent = self.runs.of(&key.0, &key.1).ok_or_else(not_started)?.agent;
        let now = (self.host.now)();
        let run = self.runs.begin(&key.0, &key.1, agent, now).await?;
        let record = self.runs.of(&key.0, &key.1);
        let run_as = Start {
            workspace: key.0.clone(),
            chat: key.1.clone(),
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
                self.chat(key).await?.orders = None;
                self.state(key, &run, RunState::Ended).await?;
                return Err(fault);
            }
        };
        let (orders, ordered) = mpsc::unbounded_channel();
        let of = RunOf {
            workspace: key.0.clone(),
            chat: key.1.clone(),
            run: run.clone(),
        };
        tokio::spawn(drive(agent, of, ordered, self.reporter.clone()));
        self.chat(key).await?.orders = Some(orders);
        self.log
            .info("run started", &[("run_id", run.as_str().into())]);
        // The agent is up: a status that can't be committed doesn't stop its turn.
        if let Err(fault) = self.state(key, &run, RunState::Starting).await {
            let fields = [
                ("run_id", run.as_str().into()),
                ("err", Field::error(&fault)),
            ];
            self.log.warn("run status not committed", &fields);
        }
        Ok(run)
    }

    fn run_of(&self, key: &Key) -> Option<RunId> {
        self.runs.of(&key.0, &key.1)?.run.clone()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use weacon_protocol::{
        ChatAction, ChatAnswerKind, ChatCommand, ChatQueryKind, Receipt, RequestId,
    };
    use weacon_testing::messages;

    use super::super::Answered;
    use super::*;
    use crate::chats::test_support::{
        Bench, FakeAgent, Started, Told, chat, client, command, parts, prompt, pushed, run, soon,
        workspace,
    };
    use crate::runs::Queued;

    fn key() -> Key {
        (workspace(), chat("c-1"))
    }

    // An engine driven by hand, on its first turn, with the client watching the chat.
    async fn on_turn() -> (Bench, Engine<FakeAgent>, Started, mpsc::Receiver<Message>) {
        let (mut bench, mut engine) = parts();
        let (client, mut pushes) = client(64);
        let subscribe = ChatQueryKind::Subscribe { after: 0 };
        engine.query(&key(), subscribe, &client).await.unwrap();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        assert_eq!(pushed(&mut pushes, 4).await.len(), 4);
        let started = soon(bench.started.recv()).await.unwrap();
        (bench, engine, started, pushes)
    }

    async fn hear(engine: &mut Engine<FakeAgent>, event: RunEvent) -> Result<(), Fault> {
        engine
            .heard(&key(), &run("run-1"), Heard::Event(event))
            .await
    }

    fn text(text: String) -> RunEvent {
        RunEvent::Text { text }
    }

    async fn latest(engine: &mut Engine<FakeAgent>) -> ChatEvent {
        let log = &engine.chat(&key()).await.unwrap().log;
        let mut window = log.window(u64::MAX, 1).await.unwrap();
        window.pop().unwrap().event
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_too_large_for_a_frame_is_committed_without_what_made_it_so() {
        let (_bench, mut engine, _agent, mut pushes) = on_turn().await;
        let large = "é".repeat(700_000);

        hear(&mut engine, text(large.clone())).await.unwrap();
        let ChatEvent::Run { update } = latest(&mut engine).await else {
            panic!("no run event");
        };
        let RunEvent::Text { text: kept } = update.event else {
            panic!("no text");
        };
        assert_eq!(kept.len(), 131_072);
        assert!(large.starts_with(&kept));

        let call = RunEvent::ToolCall {
            call: "toolu_01".to_owned(),
            tool: "Write".to_owned(),
            input: json!({ "content": large }),
        };
        hear(&mut engine, call).await.unwrap();
        let cut = RunEvent::ToolCall {
            call: "toolu_01".to_owned(),
            tool: "Write".to_owned(),
            input: Value::Null,
        };
        let update = RunUpdate {
            run: run("run-1"),
            event: cut,
        };
        assert_eq!(latest(&mut engine).await, ChatEvent::Run { update });
        assert_eq!(
            pushed(&mut pushes, 2).await,
            [
                format!("5 run-1 text {kept}"),
                "6 run-1 calls Write".to_owned()
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_too_large_to_show_is_refused_for_the_person_and_never_waits() {
        let (bench, mut engine, mut agent, mut pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let asks = RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 1,
            tool: "Write".to_owned(),
            input: json!({ "content": "a".repeat(1 << 20) }),
        };

        hear(&mut engine, asks).await.unwrap();

        assert_eq!(
            pushed(&mut pushes, 2).await,
            ["5 run-1 asks 1", "6 run-1 withdraws 1"]
        );
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer))
            if !answer.allow && answer.prompt == 1 && answer.run == run("run-1"))
        );
        let (anyone, _pushes) = client(1);
        let snapshot = ChatQueryKind::Snapshot;
        let snapshot = engine.query(&key(), snapshot, &anyone).await;
        assert!(
            matches!(snapshot, Ok(Some(ChatAnswerKind::Snapshot { waiting, runs, .. }))
            if waiting.is_empty() && runs[0].state == RunState::Running)
        );
        assert!(messages(&bench.records).contains(&"permission prompt refused unseen"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_log_write_keeps_the_agent_and_its_subscribers_start_over() {
        let (bench, mut engine, mut agent, mut pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        engine.chat(&key()).await.unwrap().log.jam();

        let refused = engine.command(&key(), prompt("r-2", "More")).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(pushed(&mut pushes, 1).await, ["events lost"]);
        assert!(engine.is_live(&key()));

        // The log was opened again: the same request now gets its receipt and its turn.
        engine.command(&key(), prompt("r-2", "More")).await.unwrap();
        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        let (again, mut caught_up) = client(64);
        let subscribe = ChatQueryKind::Subscribe { after: 4 };
        engine.query(&key(), subscribe, &again).await.unwrap();
        assert_eq!(
            pushed(&mut caught_up, 6).await,
            [
                "5 prompt r-2 More",
                "6 receipt r-2 accepted",
                "7 run-1 finished completed",
                "8 receipt r-1 completed",
                "9 run-1 is Idle",
                "10 run-1 is Running",
            ]
        );
        drop(bench);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_ends_even_when_its_last_event_cant_be_committed() {
        let (_bench, mut engine, _agent, _pushes) = on_turn().await;
        engine.chat(&key()).await.unwrap().log.jam();

        let outcome = Outcome::Completed;
        let lost = hear(&mut engine, RunEvent::Finished { outcome }).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(engine.turn(&key()), None);
        let receipt = ChatEvent::RunStatus {
            status: RunStatus {
                run: run("run-1"),
                state: RunState::Idle,
                agent: None,
            },
        };
        assert_eq!(latest(&mut engine).await, receipt);
    }

    fn entry(request: &str, text: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: prompt(request, text).request,
            text: text.to_owned(),
            repo: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_sent_again_goes_on_from_where_its_first_try_stopped() {
        let (mut bench, mut engine) = parts();
        let chat_of = (&workspace(), &chat("c-1"));
        let agent = weacon_protocol::AgentKind::Claude;

        // Stopped after the prompt reached the log, before it was queued.
        engine.commit(&key(), entry("r-1", "Hi")).await.unwrap();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        assert_eq!(engine.chat(&key()).await.unwrap().log.seq(), 4);
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        started.told.try_recv().unwrap_err();
        assert_eq!(engine.chat(&key()).await.unwrap().log.seq(), 4);
        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        // Stopped after it was queued, before it was accepted.
        engine.commit(&key(), entry("r-2", "More")).await.unwrap();
        let queued = Queued {
            request: prompt("r-2", "More").request,
            text: "More".to_owned(),
        };
        engine
            .runs
            .enqueue(chat_of, agent, queued, 1)
            .await
            .unwrap();
        engine.command(&key(), prompt("r-2", "More")).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        started.told.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_too_large_for_the_log_is_refused_and_never_runs() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );

        let large = prompt("r-2", &"a".repeat(1 << 20));
        let refused = engine.command(&key(), large).await;
        engine.command(&key(), prompt("r-3", "Next")).await.unwrap();
        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.event_too_large");
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Next".to_owned()))
        );
        assert!(engine.runs.of(&key().0, &key().1).unwrap().queue.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_agent_is_gone_doesnt_hold_up_the_next_prompt() {
        let (mut bench, mut engine, _agent, _pushes) = on_turn().await;
        // As when the agent exited and the journal couldn't be told the turn was over.
        engine.chat(&key()).await.unwrap().orders = None;

        engine.command(&key(), prompt("r-2", "More")).await.unwrap();

        let mut next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(next.start.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_that_gets_no_receipt_isnt_given_and_can_be_sent_again() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let asks = RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 1,
            tool: "Bash".to_owned(),
            input: json!({ "command": "ls" }),
        };
        hear(&mut engine, asks).await.unwrap();
        let answer = PermissionAnswer {
            run: run("run-1"),
            prompt: 1,
            allow: true,
        };
        let action = ChatAction::Permission { answer };
        let allow = || command("r-2", "c-1", action.clone());
        engine.chat(&key()).await.unwrap().log.jam();

        let refused = engine.command(&key(), allow()).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        agent.told.try_recv().unwrap_err();

        engine.command(&key(), allow()).await.unwrap();
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.allow)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_agents_session_is_kept_even_when_its_event_cant_be_committed() {
        let (_bench, mut engine, _agent, _pushes) = on_turn().await;
        let session = session();
        engine.chat(&key()).await.unwrap().log.jam();

        let started = RunEvent::Started {
            session: session.clone(),
        };
        let lost = hear(&mut engine, started).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        let record = engine.runs.of(&key().0, &key().1).unwrap();
        assert_eq!(record.session, Some(session));
    }

    fn session() -> AgentSession {
        AgentSession::try_from("0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d".to_owned()).unwrap()
    }

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
    async fn a_withdrawal_whose_write_fails_is_written_again_and_the_prompt_stops_waiting() {
        let (mut engine, _agent, _bench) = on_ask().await;
        engine.chat(&key()).await.unwrap().log.jam();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };

        hear(&mut engine, withdrawn).await.unwrap();

        assert!(waiting_in(&mut engine).await.is_empty());
        let refused = engine.command(&key(), allow("r-2").0).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_stays_open_only_while_an_agent_a_prompt_or_a_client_needs_it() {
        let (_bench, mut engine, agent, pushes) = on_turn().await;
        let other = (workspace(), chat("c-2"));
        let (watcher, watching) = client(4);
        let subscribe = || ChatQueryKind::Subscribe { after: 0 };
        engine.query(&other, subscribe(), &watcher).await.unwrap();

        engine.settle(&key());
        engine.settle(&other);
        assert!(engine.chats.contains_key(&key()) && engine.chats.contains_key(&other));

        // The agent exits, and both clients hang up.
        drop(agent);
        engine
            .heard(&key(), &run("run-1"), Heard::Exited(None))
            .await
            .unwrap();
        engine.settle(&key());
        assert!(
            engine.chats.contains_key(&key()),
            "a client still watches it"
        );
        drop((pushes, watching));
        engine.settle(&key());
        assert!(!engine.chats.contains_key(&key()));

        // A hung-up client's chat is let go when anyone subscribes anywhere.
        assert!(engine.chats.contains_key(&other));
        let (third, _watching) = client(4);
        let elsewhere = (workspace(), chat("c-3"));
        engine.query(&elsewhere, subscribe(), &third).await.unwrap();
        assert!(!engine.chats.contains_key(&other));
        assert!(engine.chats.contains_key(&elsewhere));
    }

    #[tokio::test(start_paused = true)]
    async fn what_another_run_says_is_ignored_and_a_read_failure_is_logged_once() {
        let (bench, mut engine, _agent, _pushes) = on_turn().await;
        let stale = Heard::Event(text("Old".to_owned()));
        engine.heard(&key(), &run("run-9"), stale).await.unwrap();
        let other = (workspace(), chat("c-2"));
        let elsewhere = Heard::Event(text("Other".to_owned()));
        engine
            .heard(&other, &run("run-1"), elsewhere)
            .await
            .unwrap();
        assert_eq!(engine.chat(&key()).await.unwrap().log.seq(), 4);
        assert!(!engine.chats.contains_key(&other));

        let fault = Fault::new(code!("agent.io_failed"), "The agent's pipes failed");
        let gone = Heard::Exited(Some(fault));
        engine.heard(&key(), &run("run-1"), gone).await.unwrap();

        assert!(!engine.is_live(&key()));
        assert_eq!(engine.turn(&key()), None);
        let logged = messages(&bench.records);
        assert_eq!(
            logged
                .iter()
                .filter(|msg| **msg == "run output not read")
                .count(),
            1
        );
        engine
            .heard(&key(), &run("run-1"), Heard::Exited(None))
            .await
            .unwrap();
        assert_eq!(engine.chat(&key()).await.unwrap().log.seq(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_that_stopped_reading_gets_no_turn_and_the_prompt_goes_to_a_new_run() {
        let (mut bench, mut engine, agent, _pushes) = on_turn().await;
        engine.command(&key(), prompt("r-2", "More")).await.unwrap();
        // The agent exits; its report of that isn't handled yet.
        drop(agent.say);
        let mut tries = 0;
        while engine.takes_orders(&key()) && tries < 1000 {
            tries += 1;
            tokio::task::yield_now().await;
        }
        assert!(!engine.takes_orders(&key()));

        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        let mut next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(next.start.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        // The old run's exit, heard late, leaves the new one alone.
        let exited = Heard::Exited(None);
        engine.heard(&key(), &run("run-1"), exited).await.unwrap();
        assert!(engine.takes_orders(&key()));
        assert_eq!(engine.turn(&key()), Some(prompt("r-2", "More").request));
    }

    // A run on its turn, waiting for a person to answer its prompt 1.
    async fn on_ask() -> (Engine<FakeAgent>, Started, Bench) {
        let (bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let asks = RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 1,
            tool: "Write".to_owned(),
            input: json!({}),
        };
        hear(&mut engine, asks).await.unwrap();
        (engine, agent, bench)
    }

    // What the chat's snapshot says waits for a person.
    async fn waiting_in(engine: &mut Engine<FakeAgent>) -> Vec<u64> {
        let (anyone, _pushes) = client(1);
        let snapshot = engine.query(&key(), ChatQueryKind::Snapshot, &anyone).await;
        let Ok(Some(ChatAnswerKind::Snapshot { waiting, .. })) = snapshot else {
            panic!("no snapshot");
        };
        waiting
    }

    #[tokio::test(start_paused = true)]
    async fn a_working_run_with_no_turn_left_after_a_restart_sleeps() {
        let (_bench, mut engine, _agent, _pushes) = on_turn().await;
        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        // As when the core stopped right after starting a run, before it got a turn.
        let (run_1, running) = (run("run-1"), RunState::Running);
        let status = RunStatus {
            run: run_1,
            state: running,
            agent: None,
        };
        engine
            .commit(&key(), ChatEvent::RunStatus { status })
            .await
            .unwrap();
        engine.chats.remove(&key());

        waiting_in(&mut engine).await;

        let status = RunStatus {
            run: run("run-1"),
            state: RunState::Sleeping,
            agent: None,
        };
        assert_eq!(latest(&mut engine).await, ChatEvent::RunStatus { status });
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_cut_off_by_a_restart_ends_and_leaves_no_prompt_waiting() {
        let (mut engine, _agent, _bench) = on_ask().await;
        assert_eq!(waiting_in(&mut engine).await.len(), 1);
        // As after a restart: the log is all that is left of the chat.
        engine.chats.remove(&key());

        assert!(waiting_in(&mut engine).await.is_empty());
        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut engine, &first).await,
            [Outcome::Accepted, Outcome::Unknown]
        );
        let status = RunStatus {
            run: run("run-1"),
            state: RunState::Sleeping,
            agent: None,
        };
        assert_eq!(latest(&mut engine).await, ChatEvent::RunStatus { status });
    }

    // The outcomes of `request`'s receipts in the log, in order.
    async fn outcomes(engine: &mut Engine<FakeAgent>, request: &RequestId) -> Vec<Outcome> {
        let log = &engine.chat(&key()).await.unwrap().log;
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

    fn allow(request: &str) -> (ChatCommand, PermissionAnswer) {
        let answer = PermissionAnswer {
            run: run("run-1"),
            prompt: 1,
            allow: true,
        };
        let action = ChatAction::Permission {
            answer: answer.clone(),
        };
        (command(request, "c-1", action), answer)
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_whose_receipt_was_written_reaches_its_run_when_sent_again() {
        let (mut engine, mut agent, _bench) = on_ask().await;
        let (answers, answer) = allow("r-2");
        engine.chat(&key()).await.unwrap().log.jam();
        let failed = engine.command(&key(), answers.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        // As when its receipt reached the disk all the same.
        let request = answers.request.clone();
        engine
            .receipt(&key(), &request, Outcome::Completed)
            .await
            .unwrap();

        engine.command(&key(), answers.clone()).await.unwrap();
        engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(agent.told.recv()).await, Some(Told::Answer(answer)));
        assert_eq!(soon(agent.told.recv()).await, None);
    }

    // An answer to prompt 1 whose receipt is written but which hasn't reached the run.
    async fn on_its_way(engine: &mut Engine<FakeAgent>) -> ChatCommand {
        let (answers, answer) = allow("r-2");
        let request = answers.request.clone();
        let answered = Answered {
            request: request.clone(),
            answer,
        };
        engine.chat(&key()).await.unwrap().answers.push(answered);
        let receipt = Outcome::Completed;
        engine.receipt(&key(), &request, receipt).await.unwrap();
        answers
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_naming_another_run_gives_nothing_and_changes_no_run() {
        let (mut engine, mut agent, _bench) = on_ask().await;
        let mut answers = on_its_way(&mut engine).await;
        if let ChatAction::Permission { answer } = &mut answers.action {
            answer.run = run("run-9");
        }

        engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(agent.told.recv()).await, None);
        let runs = &engine.chat(&key()).await.unwrap().log.runs().runs;
        assert_eq!(runs[0].run, run("run-1"));
        assert_eq!(waiting_in(&mut engine).await.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_on_its_way_to_a_withdrawn_prompt_is_never_given() {
        let (mut engine, mut agent, _bench) = on_ask().await;
        let answers = on_its_way(&mut engine).await;
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };
        hear(&mut engine, withdrawn).await.unwrap();

        engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawal_that_cant_be_written_still_drops_the_answers_on_their_way() {
        let (mut engine, _agent, _bench) = on_ask().await;
        on_its_way(&mut engine).await;
        // Both writes fail: the log is broken, and a folder where its file was keeps it so.
        let file = engine.workspaces.root(&key().0).join("chats/c-c-1.jsonl");
        std::fs::rename(&file, file.with_extension("moved")).unwrap();
        std::fs::create_dir(&file).unwrap();
        engine.chat(&key()).await.unwrap().log.jam();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };

        let lost = hear(&mut engine, withdrawn).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        assert!(engine.chat(&key()).await.unwrap().answers.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_prompt_whose_write_landed_is_withdrawn_in_the_log() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        // As when the prompt reached the disk but its write was reported failed.
        let asks = RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 1,
            tool: "Write".to_owned(),
            input: json!({}),
        };
        hear(&mut engine, asks).await.unwrap();
        let failed = Fault::new(
            code!("chat.log_not_written"),
            "The chat's log can't be written",
        );

        let refused = engine.asked(&key(), &run("run-1"), 1, Err(failed)).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if !answer.allow)
        );
        assert!(waiting_in(&mut engine).await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_already_in_the_log_reaches_its_run_once_and_is_logged_once() {
        let (mut engine, mut agent, _bench) = on_ask().await;
        let (answers, answer) = allow("r-2");
        let answered = Answered {
            request: answers.request.clone(),
            answer: PermissionAnswer {
                run: run("run-1"),
                prompt: 1,
                allow: true,
            },
        };
        // As when its receipt and its answer reached the disk but the write was reported failed.
        engine.chat(&key()).await.unwrap().answers.push(answered);
        let receipt = Outcome::Completed;
        engine
            .receipt(&key(), &answers.request, receipt)
            .await
            .unwrap();
        let event = RunEvent::PermissionAnswered {
            answer: answer.clone(),
        };
        hear(&mut engine, event).await.unwrap();

        engine.command(&key(), answers.clone()).await.unwrap();
        engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(agent.told.recv()).await, Some(Told::Answer(answer)));
        assert_eq!(soon(agent.told.recv()).await, None);
        let log = &engine.chat(&key()).await.unwrap().log;
        let entries = log.window(u64::MAX, 64).await.unwrap();
        let logged = entries.iter().filter(|entry| {
            matches!(&entry.event, ChatEvent::Run { update } if matches!(update.event, RunEvent::PermissionAnswered { .. }))
        });
        assert_eq!(logged.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_of_a_run_cut_off_by_a_restart_takes_no_answer() {
        let (mut engine, _agent, _bench) = on_ask().await;
        // As after a restart: the log still shows the prompt, but its run is gone.
        engine.chats.remove(&key());

        let refused = engine.command(&key(), allow("r-2").0).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn answering_one_prompt_keeps_another_answer_still_on_its_way() {
        let (mut engine, mut agent, _bench) = on_ask().await;
        let asks = RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 2,
            tool: "Write".to_owned(),
            input: json!({}),
        };
        hear(&mut engine, asks).await.unwrap();
        let (second, given) = allow("r-2");
        let second = ChatCommand {
            action: ChatAction::Permission {
                answer: PermissionAnswer {
                    prompt: 2,
                    ..given.clone()
                },
            },
            ..second
        };
        // As when its receipt was written but its delivery was cut short.
        let pending = Answered {
            request: second.request.clone(),
            answer: PermissionAnswer {
                run: run("run-1"),
                prompt: 2,
                allow: true,
            },
        };
        engine.chat(&key()).await.unwrap().answers.push(pending);
        let receipt = Outcome::Completed;
        engine
            .receipt(&key(), &second.request, receipt)
            .await
            .unwrap();

        engine.command(&key(), allow("r-3").0).await.unwrap();
        engine.command(&key(), second).await.unwrap();

        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Answer(given.clone()))
        );
        let two = PermissionAnswer { prompt: 2, ..given };
        assert_eq!(soon(agent.told.recv()).await, Some(Told::Answer(two)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_under_another_commands_request_never_reaches_the_run() {
        let (mut engine, mut agent, _bench) = on_ask().await;
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        engine.command(&key(), interrupt).await.unwrap();
        assert_eq!(soon(agent.told.recv()).await, Some(Told::Interrupt));

        engine.command(&key(), allow("r-2").0).await.unwrap();

        assert_eq!(soon(agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_whose_receipt_was_written_stops_its_turn_when_sent_again() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        engine.chat(&key()).await.unwrap().log.jam();
        let failed = engine.command(&key(), interrupt.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        // As when its receipt reached the disk all the same.
        let request = interrupt.request.clone();
        engine
            .receipt(&key(), &request, Outcome::Completed)
            .await
            .unwrap();

        engine.command(&key(), interrupt.clone()).await.unwrap();
        engine.command(&key(), interrupt).await.unwrap();

        assert_eq!(soon(agent.told.recv()).await, Some(Told::Interrupt));
        assert_eq!(soon(agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_that_cant_be_committed_isnt_sent() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        engine.chat(&key()).await.unwrap().log.jam();

        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        let refused = engine.command(&key(), interrupt).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(soon(agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_receipt_cant_be_committed_ends_as_reported_on_the_next_prompt() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let request = prompt("r-1", "Hi").request;
        engine.chat(&key()).await.unwrap().log.jam();
        let (run_1, idle) = (run("run-1"), RunState::Idle);
        let lost = engine
            .finished(&key(), &run_1, Outcome::Completed, idle)
            .await;
        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(engine.turn(&key()), Some(request.clone()));

        engine.command(&key(), prompt("r-2", "More")).await.unwrap();

        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        assert_eq!(
            outcomes(&mut engine, &request).await,
            [Outcome::Accepted, Outcome::Completed]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_prompt_whose_end_is_in_the_log_never_runs() {
        let (_bench, mut engine, mut agent, _pushes) = on_turn().await;
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        engine.command(&key(), prompt("r-2", "More")).await.unwrap();
        // As when its failure was committed but it couldn't leave the queue.
        let code = code!("agent.not_started");
        let request = prompt("r-2", "More").request;
        engine
            .receipt(&key(), &request, Outcome::Failed { code })
            .await
            .unwrap();

        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        assert_eq!(soon(agent.told.recv()).await, None);
        assert!(engine.runs.of(&key().0, &key().1).unwrap().queue.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_exit_whose_receipt_failed_is_ended_before_the_next_run_starts() {
        let (mut bench, mut engine, agent, _pushes) = on_turn().await;
        engine.chat(&key()).await.unwrap().log.jam();
        drop(agent);
        let exited = Heard::Exited(None);
        let lost = engine.heard(&key(), &run("run-1"), exited).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        // Its outcome keeps the chat open, though nothing else does.
        engine.settle(&key());
        assert!(engine.chats.contains_key(&key()));

        engine.command(&key(), prompt("r-2", "More")).await.unwrap();

        let mut next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(next.start.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut engine, &first).await,
            [Outcome::Accepted, Outcome::Unknown]
        );
        assert!(engine.chat(&key()).await.unwrap().unended.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn an_outcome_left_by_another_turn_doesnt_end_this_one() {
        let (_bench, mut engine, _agent, _pushes) = on_turn().await;
        let other = (prompt("r-0", "Earlier").request, Outcome::Completed);
        engine.chat(&key()).await.unwrap().unended = Some(other);

        let (run_1, idle) = (run("run-1"), RunState::Idle);
        engine
            .finished(&key(), &run_1, Outcome::Interrupted, idle)
            .await
            .unwrap();

        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut engine, &first).await,
            [Outcome::Accepted, Outcome::Interrupted]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_whose_run_is_gone_ends_unknown_on_the_next_prompt() {
        let (mut bench, mut engine, _agent, _pushes) = on_turn().await;
        // As after a restart: nothing of the chat or its run is in memory.
        engine.chats.remove(&key());

        engine.command(&key(), prompt("r-2", "More")).await.unwrap();

        let first = prompt("r-1", "Hi").request;
        assert_eq!(
            outcomes(&mut engine, &first).await,
            [Outcome::Accepted, Outcome::Unknown]
        );
        let mut next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(next.start.run, run("run-2"));
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_its_run_never_got_goes_back_to_the_queue() {
        let (_bench, mut engine, _agent, _pushes) = on_turn().await;
        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        // The run stopped reading while its next turn was being saved.
        let (closed, gone) = mpsc::unbounded_channel();
        drop(gone);
        engine.chat(&key()).await.unwrap().orders = Some(closed);
        let next = Queued {
            request: prompt("r-2", "More").request,
            text: "More".to_owned(),
        };

        engine
            .give_turn(&key(), &run("run-1"), next.clone())
            .await
            .unwrap();

        let record = engine.runs.of(&key().0, &key().1).unwrap();
        assert_eq!(
            (record.turn.as_ref(), record.queue.as_slice()),
            (None, [next].as_slice())
        );
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
