//! What the engine does with a run: start it, give it its next turn, and commit what it says.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatEntry, ChatEvent, Message, Outcome, PermissionAnswer, RunEvent, RunId, RunState, RunUpdate,
    code,
};

use super::{Engine, Key, Start, Waiting};
use crate::runs::task::{Agent, Heard, Order, RunOf, drive};

// What is kept of a text too long for one frame: it still fits when every byte is escaped.
const TEXT_KEPT: usize = 128 << 10;

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
                let kept = self
                    .runs
                    .update(&key.0, &key.1, |record| record.session = Some(session))
                    .await;
                committed.and(kept)
            }
            RunEvent::PermissionAsked { prompt, .. } => {
                self.asked(key, run, prompt, committed).await
            }
            RunEvent::PermissionWithdrawn { prompt, .. } => {
                committed?;
                let waiting = &mut self.chat(key).await?.waiting;
                waiting.retain(|waits| waits.prompt != prompt);
                if waiting.is_empty() && self.turn(key).is_some() {
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
        if let Ok((entry, true)) = &committed {
            let seq = entry.seq;
            self.chat(key).await?.waiting.push(Waiting { prompt, seq });
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
        committed?;
        let event = RunEvent::PermissionWithdrawn {
            run: run.clone(),
            prompt,
        };
        self.commit_said(key, run, &event).await.map(drop)
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
        self.chat(key).await?.waiting.clear();
        self.runs
            .update(&key.0, &key.1, |record| {
                record.turn = None;
                record.active_at = now;
            })
            .await?;
        if let Some(request) = turn {
            self.receipt(key, &request, outcome).await?;
        }
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
        let run = match self.run_of(key).filter(|_| self.is_live(key)) {
            Some(run) => run,
            None => match self.launch(key).await {
                Ok(run) => run,
                Err(fault) => return self.fail_queue(key, &fault).await,
            },
        };
        let now = (self.host.now)();
        self.runs
            .update(&key.0, &key.1, |record| {
                record.queue.retain(|queued| queued.request != next.request);
                record.turn = Some(next.request);
                record.active_at = now;
            })
            .await?;
        self.order(key, Order::Send(next.text));
        self.state(key, &run, RunState::Running).await
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
            self.runs
                .update(&key.0, &key.1, |record| {
                    record.queue.retain(|waits| waits.request != queued.request);
                })
                .await?;
        }
        Ok(())
    }

    // Starts the chat's agent as a new run, resuming its session when it has one.
    async fn launch(&mut self, key: &Key) -> Result<RunId, Fault> {
        let not_started = || Fault::new(code!("agent.not_started"), "The agent couldn't start");
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

// `event` without what made it too large for one frame.
fn cut(event: &RunEvent) -> RunEvent {
    match event.clone() {
        RunEvent::Text { text } => {
            let kept = text
                .char_indices()
                .take_while(|(at, _)| *at < TEXT_KEPT)
                .map(|(_, letter)| letter);
            RunEvent::Text {
                text: kept.collect(),
            }
        }
        RunEvent::ToolCall { call, tool, .. } => RunEvent::ToolCall {
            call,
            tool,
            input: Value::Null,
        },
        RunEvent::PermissionAsked {
            run, prompt, tool, ..
        } => RunEvent::PermissionAsked {
            run,
            prompt,
            tool,
            input: Value::Null,
        },
        whole @ (RunEvent::Started { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ToolDone { .. }
        | RunEvent::PermissionWithdrawn { .. }
        | RunEvent::Finished { .. }
        | RunEvent::Newer) => whole,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{ChatAnswerKind, ChatQueryKind};
    use weacon_testing::messages;

    use super::*;
    use crate::chats::test_support::{
        Bench, FakeAgent, Started, Told, chat, client, parts, prompt, pushed, run, soon, workspace,
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
            status: weacon_protocol::RunStatus {
                run: run("run-1"),
                state: RunState::Idle,
            },
        };
        assert_eq!(latest(&mut engine).await, receipt);
    }

    fn entry(request: &str, text: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: prompt(request, text).request,
            text: text.to_owned(),
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
        let action = weacon_protocol::ChatAction::Permission { answer };
        let allow = || crate::chats::test_support::command("r-2", "c-1", action.clone());
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
        let session = weacon_protocol::AgentSession::try_from(
            "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d".to_owned(),
        )
        .unwrap();
        engine.chat(&key()).await.unwrap().log.jam();

        let started = RunEvent::Started {
            session: session.clone(),
        };
        let lost = hear(&mut engine, started).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        let record = engine.runs.of(&key().0, &key().1).unwrap();
        assert_eq!(record.session, Some(session));
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
}
