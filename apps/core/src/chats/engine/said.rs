//! What the engine does with what a run says: commit it, then act on it.

use weacon_observability::Fault;
use weacon_protocol::{
    ChatEvent, Message, Outcome, PermissionAnswer, RunEvent, RunId, RunState, RunUpdate, code,
};

use super::agent::{Agent, Heard, Order};
use super::cut::cut;
use super::{ChatKey, Engine, about, failed};

/// How a run's event reached the log: whole, or cut down to fit one frame.
enum Logged {
    Whole,
    Cut,
}

impl<A: Agent> Engine<A> {
    pub(super) async fn heard(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        heard: Heard,
    ) -> Result<(), Fault> {
        if !self.is_live(key) || self.run_of(key).as_ref() != Some(run) {
            return Ok(());
        }
        match heard {
            Heard::Exited(fault) => {
                if let Some(fault) = fault {
                    let fields = failed(key, run, &fault);
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
    async fn said(&mut self, key: &ChatKey, run: &RunId, event: RunEvent) -> Result<(), Fault> {
        let committed = self.commit_said(key, run, &event).await;
        match event {
            // Kept whether or not the event was committed: the chat's next run resumes it.
            RunEvent::Started { session } => {
                let kept = self.keep_session(key, session).await;
                committed.and(kept)
            }
            RunEvent::PermissionAsked { prompt, .. } => {
                self.permission_asked(key, run, prompt, committed).await
            }
            RunEvent::PermissionWithdrawn { prompt, .. } => {
                self.permission_withdrawn(key, run, prompt, committed.map(drop))
                    .await
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
                let ended = self.end_turn(key, run, outcome).await;
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

    // The agent no longer asks. An answer still on its way is never given, and a failed write
    // is tried again, which reopens the log, unless the log has the withdrawal all the same.
    async fn permission_withdrawn(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        prompt: u32,
        committed: Result<(), Fault>,
    ) -> Result<(), Fault> {
        let answers = &mut self.open(key).await?.pending_answers;
        answers.retain(|answered| answered.answer.prompt != prompt);
        let logged = !self.open(key).await?.events.runs().waits(run, prompt);
        if committed.is_err() && !logged {
            let event = RunEvent::PermissionWithdrawn {
                run: run.clone(),
                prompt,
            };
            self.commit_said(key, run, &event).await?;
        }
        self.running_again(key, run).await
    }

    // A prompt a person can't be shown in full is refused for them; it never waits unseen.
    async fn permission_asked(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        prompt: u32,
        committed: Result<Logged, Fault>,
    ) -> Result<(), Fault> {
        if matches!(committed, Ok(Logged::Whole)) {
            return self.set_state(key, run, RunState::Waiting).await;
        }
        let answer = PermissionAnswer {
            run: run.clone(),
            prompt,
            allow: false,
        };
        self.order(key, Order::Answer(answer));
        self.log
            .warn("permission prompt refused unseen", &about(key, run));
        // Withdrawn wherever it reached the log, even by a write reported failed.
        let logged = self.open(key).await?.events.runs().waits(run, prompt);
        if committed.is_ok() || logged {
            let event = RunEvent::PermissionWithdrawn {
                run: run.clone(),
                prompt,
            };
            self.commit_said(key, run, &event).await?;
        }
        committed.map(drop)
    }

    // Commits a run's event, cut down when it is too large for one frame.
    async fn commit_said(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        event: &RunEvent,
    ) -> Result<Logged, Fault> {
        let entry = |event: RunEvent| ChatEvent::Run {
            update: RunUpdate {
                run: run.clone(),
                event,
            },
        };
        match self.commit(key, entry(event.clone())).await {
            Err(fault) if fault.code() == &code!("chat.event_too_large") => {
                self.commit(key, entry(cut(event))).await?;
                Ok(Logged::Cut)
            }
            committed => committed.map(|()| Logged::Whole),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use weacon_protocol::{ChatAnswerKind, ChatQueryKind};
    use weacon_testing::messages;

    use super::super::by_hand::{
        OnTurn, allow_one, hear, latest, on_ask, on_its_way, on_turn, waiting_in,
    };
    use super::*;
    use crate::chats::test_support::{
        Told, asks, chat, client, key, prompt, pushed, run, session, soon, text,
    };

    // The workspace, chat, run and error named by the first line logged as `msg`.
    fn named(on: &OnTurn, msg: &str) -> [Value; 4] {
        let records = on.bench.records.lock().unwrap();
        let line = records.iter().find(|record| record.msg == msg).unwrap();
        ["workspace_id", "chat_id", "run_id", "err"]
            .map(|name| line.fields.get(name).cloned().unwrap_or(Value::Null))
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_too_large_for_a_frame_is_committed_without_what_made_it_so() {
        let mut on = on_turn().await;
        let large = "é".repeat(700_000);

        hear(&mut on.engine, text(&large)).await.unwrap();
        let ChatEvent::Run { update } = latest(&mut on.engine).await else {
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
        hear(&mut on.engine, call).await.unwrap();
        let cut = RunEvent::ToolCall {
            call: "toolu_01".to_owned(),
            tool: "Write".to_owned(),
            input: Value::Null,
        };
        let update = RunUpdate {
            run: run("run-1"),
            event: cut,
        };
        assert_eq!(latest(&mut on.engine).await, ChatEvent::Run { update });
        assert_eq!(
            pushed(&mut on.pushes, 2).await,
            [
                format!("5 run-1 text {kept}"),
                "6 run-1 calls Write".to_owned()
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_too_large_to_show_is_refused_for_the_person_and_never_waits() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let asks = RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 1,
            tool: "Write".to_owned(),
            input: json!({ "content": "a".repeat(1 << 20) }),
        };

        hear(&mut on.engine, asks).await.unwrap();

        assert_eq!(
            pushed(&mut on.pushes, 2).await,
            ["5 run-1 asks 1", "6 run-1 withdraws 1"]
        );
        assert!(
            matches!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer))
            if !answer.allow && answer.prompt == 1 && answer.run == run("run-1"))
        );
        let (anyone, _pushes) = client(1);
        let snapshot = ChatQueryKind::Snapshot;
        let snapshot = on.engine.query(&key(), snapshot, &anyone).await;
        assert!(
            matches!(snapshot, Ok(Some(ChatAnswerKind::Snapshot { waiting, runs, .. }))
            if waiting.is_empty() && runs[0].state == RunState::Running)
        );
        assert!(messages(&on.bench.records).contains(&"permission prompt refused unseen"));
        let named = named(&on, "permission prompt refused unseen");
        assert_eq!(
            named,
            [json!("w-1"), json!("c-1"), json!("run-1"), Value::Null]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_log_write_keeps_the_agent_and_its_subscribers_start_over() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        on.engine.open(&key()).await.unwrap().events.jam();

        let refused = on.engine.command(&key(), prompt("r-2", "More")).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(pushed(&mut on.pushes, 1).await, ["events lost"]);
        assert!(on.engine.is_live(&key()));

        // The log was opened again: the same request now gets its receipt and its turn.
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        let (again, mut caught_up) = client(64);
        let subscribe = ChatQueryKind::Subscribe { after: 4 };
        on.engine.query(&key(), subscribe, &again).await.unwrap();
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
        drop(on.bench);
    }

    #[tokio::test(start_paused = true)]
    async fn an_agents_session_is_kept_even_when_its_event_cant_be_committed() {
        let mut on = on_turn().await;
        let session = session();
        on.engine.open(&key()).await.unwrap().events.jam();

        let started = RunEvent::Started {
            session: session.clone(),
        };
        let lost = hear(&mut on.engine, started).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        let record = on.engine.runs.of(&key()).unwrap();
        assert_eq!(record.session, Some(session));
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawal_whose_write_fails_is_written_again_and_the_prompt_stops_waiting() {
        let mut on = on_ask().await;
        on.engine.open(&key()).await.unwrap().events.jam();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };

        hear(&mut on.engine, withdrawn).await.unwrap();

        assert!(waiting_in(&mut on.engine).await.is_empty());
        let refused = on.engine.command(&key(), allow_one("r-2").0).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn what_another_run_says_is_ignored_and_a_read_failure_is_logged_once() {
        let mut on = on_turn().await;
        let stale = Heard::Event(text("Old"));
        on.engine.heard(&key(), &run("run-9"), stale).await.unwrap();
        let other = ChatKey {
            chat: chat("c-2"),
            ..key()
        };
        let elsewhere = Heard::Event(text("Other"));
        on.engine
            .heard(&other, &run("run-1"), elsewhere)
            .await
            .unwrap();
        assert_eq!(on.engine.open(&key()).await.unwrap().events.seq(), 4);
        assert!(!on.engine.open_chats.contains_key(&other));

        let fault = Fault::new(code!("agent.io_failed"), "The agent's pipes failed");
        let gone = Heard::Exited(Some(fault));
        on.engine.heard(&key(), &run("run-1"), gone).await.unwrap();

        assert!(!on.engine.is_live(&key()));
        assert_eq!(on.engine.turn(&key()), None);
        let logged = messages(&on.bench.records);
        assert_eq!(
            logged
                .iter()
                .filter(|msg| **msg == "run output not read")
                .count(),
            1
        );
        let [workspace, chat, run_id, err] = named(&on, "run output not read");
        assert_eq!(
            [workspace, chat, run_id],
            [json!("w-1"), json!("c-1"), json!("run-1")]
        );
        assert_eq!(err["code"], "agent.io_failed");
        on.engine
            .heard(&key(), &run("run-1"), Heard::Exited(None))
            .await
            .unwrap();
        assert_eq!(on.engine.open(&key()).await.unwrap().events.seq(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_on_its_way_to_a_withdrawn_prompt_is_never_given() {
        let mut on = on_ask().await;
        let (answers, _) = on_its_way(&mut on.engine, 1).await;
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };
        hear(&mut on.engine, withdrawn).await.unwrap();

        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawal_that_cant_be_written_still_drops_the_answers_on_their_way() {
        let mut on = on_ask().await;
        on_its_way(&mut on.engine, 1).await;
        // Both writes fail: the log is broken, and a folder where its file was keeps it so.
        let file = on
            .engine
            .workspaces
            .root(&key().workspace)
            .join("chats/c-c-1.jsonl");
        std::fs::rename(&file, file.with_extension("moved")).unwrap();
        std::fs::create_dir(&file).unwrap();
        on.engine.open(&key()).await.unwrap().events.jam();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };

        let lost = hear(&mut on.engine, withdrawn).await;

        assert_eq!(lost.unwrap_err().code().as_str(), "chat.log_not_written");
        assert!(
            on.engine
                .open(&key())
                .await
                .unwrap()
                .pending_answers
                .is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawal_whose_write_landed_is_not_written_again() {
        let mut on = on_ask().await;
        // As when the withdrawal reached the disk but its write was reported failed.
        let event = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };
        hear(&mut on.engine, event).await.unwrap();
        let failed = Fault::new(
            code!("chat.log_not_written"),
            "The chat's log can't be written",
        );

        on.engine
            .permission_withdrawn(&key(), &run("run-1"), 1, Err(failed))
            .await
            .unwrap();

        let log = &on.engine.open(&key()).await.unwrap().events;
        let entries = log.window(u64::MAX, 64).await.unwrap();
        let withdrawals = entries.iter().filter(|entry| {
            matches!(&entry.event, ChatEvent::Run { update } if matches!(update.event, RunEvent::PermissionWithdrawn { .. }))
        });
        assert_eq!(withdrawals.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_permission_whose_write_landed_is_withdrawn_in_the_log() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        // As when the prompt reached the disk but its write was reported failed.
        let asks = asks("run-1", 1);
        hear(&mut on.engine, asks).await.unwrap();
        let failed = Fault::new(
            code!("chat.log_not_written"),
            "The chat's log can't be written",
        );

        let refused = on
            .engine
            .permission_asked(&key(), &run("run-1"), 1, Err(failed))
            .await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert!(
            matches!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)) if !answer.allow)
        );
        assert!(waiting_in(&mut on.engine).await.is_empty());
    }
}
