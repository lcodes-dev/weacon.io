//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, Outcome, PermissionAnswer, RequestId, RunEvent,
    RunUpdate, code,
};

use super::agent::{Agent, Order};
use super::{ChatKey, Engine, PendingAnswer, PendingInterrupt, unsupported};
use crate::chats::log::Seen;
use crate::runs::Queued;

impl<A: Agent> Engine<A> {
    pub(super) async fn command(
        &mut self,
        key: &ChatKey,
        command: ChatCommand,
    ) -> Result<(), Fault> {
        let request = command.request;
        let action = command.action;
        let seen = self.open(key).await?.events.seen(&request, &action).await?;
        match seen {
            Seen::Same => self.resume(key, &request, action).await,
            Seen::Other => Err(Fault::new(
                code!("request.conflict"),
                "That request id was used for another command",
            )),
            Seen::Never | Seen::Pending => self.act(key, request, action, seen).await,
        }
    }

    // Whatever cut the first try short, what it began still happens: an answer still waiting
    // reaches its run, and a prompt it queued gets its turn.
    async fn resume(
        &mut self,
        key: &ChatKey,
        request: &RequestId,
        action: ChatAction,
    ) -> Result<(), Fault> {
        match action {
            ChatAction::Permission { answer } => self.deliver(key, request, answer).await,
            ChatAction::Interrupt => self.stop_turn(key, request).await,
            ChatAction::Prompt { .. } | ChatAction::Newer => self.advance(key).await,
        }
    }

    async fn act(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        action: ChatAction,
        seen: Seen,
    ) -> Result<(), Fault> {
        // Before the log, so a chat a newer core runs gets nothing from this one.
        self.runs.refuse_newer(key)?;
        match action {
            ChatAction::Prompt {
                text,
                repo,
                agent: named,
                ..
            } => {
                let (agent, fresh) = (self.agent_of(key, named).await?, seen != Seen::Pending);
                if fresh && text.trim().is_empty() {
                    return Err(Fault::new(
                        code!("chat.empty_prompt"),
                        "The prompt is empty",
                    ));
                }
                if fresh {
                    let prompt = ChatEvent::Prompt {
                        request: request.clone(),
                        text: text.clone(),
                        repo,
                        agent: named,
                    };
                    self.commit(key, prompt).await?;
                }
                self.prompt(key, request, text, agent).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            ChatAction::Newer => Err(unsupported()),
        }
    }

    // The chat's agent is the one its first logged prompt named; a later prompt can't change it.
    async fn agent_of(
        &mut self,
        key: &ChatKey,
        named: Option<AgentKind>,
    ) -> Result<AgentKind, Fault> {
        let chosen = self.open(key).await?.events.agent();
        match (named, chosen) {
            (Some(AgentKind::Newer), _) => Err(unsupported()),
            (Some(named), Some(chosen)) if named != chosen => Err(Fault::new(
                code!("chat.agent_conflict"),
                "The chat already runs another agent",
            )),
            (named, chosen) => Ok(chosen.or(named).unwrap_or(AgentKind::Claude)),
        }
    }

    async fn prompt(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        text: String,
        agent: AgentKind,
    ) -> Result<(), Fault> {
        let queued = Queued {
            request: request.clone(),
            text,
        };
        self.runs.enqueue(key, agent, queued, self.now()).await?;
        self.commit_receipt(key, &request, Outcome::Accepted)
            .await?;
        self.advance(key).await
    }

    // The answer reaches only the chat's own run, and only for a prompt still waiting. Its
    // receipt is committed before the agent is told, so a refused answer was never given.
    async fn permission(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        answer: PermissionAnswer,
    ) -> Result<(), Fault> {
        let live = self.is_live(key);
        let chat = self.open(key).await?;
        // The log forgets another run's prompts as soon as a newer run reports.
        if !live || !chat.events.runs().waits(&answer.run, answer.prompt) {
            return Err(Fault::new(
                code!("agent.no_such_prompt"),
                "That permission prompt isn't waiting",
            ));
        }
        // Kept before its receipt is written, for a write reported failed that still landed; an
        // earlier answer to the same prompt still on its way goes only once this one's is in.
        chat.pending_answers.push(PendingAnswer {
            request: request.clone(),
            answer: answer.clone(),
        });
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        let answers = &mut self.open(key).await?.pending_answers;
        answers.retain(|other| other.answer.prompt != answer.prompt || other.request == request);
        self.deliver(key, &request, answer).await
    }

    // Gives the answer `request` committed to its run. It is logged unless the log has it
    // already, as after a write reported failed that still landed.
    async fn deliver(
        &mut self,
        key: &ChatKey,
        request: &RequestId,
        answer: PermissionAnswer,
    ) -> Result<(), Fault> {
        let live = self.is_live(key);
        let chat = self.open(key).await?;
        let pending = |other: &PendingAnswer| other.request == *request && other.answer == answer;
        if !live {
            return Ok(());
        }
        // Given already: only its run's state may still need saving.
        if !chat.pending_answers.iter().any(pending) {
            return self.running_again(key, &answer.run).await;
        }
        if chat.events.runs().waits(&answer.run, answer.prompt) {
            // In the log before the agent is told, so a crash between the two leaves the truth.
            let update = RunUpdate {
                run: answer.run.clone(),
                event: RunEvent::PermissionAnswered {
                    answer: answer.clone(),
                },
            };
            self.commit(key, ChatEvent::Run { update }).await?;
        }
        let chat = self.open(key).await?;
        // The prompt has its answer: any other on its way to it is never given.
        let answered = |other: &PendingAnswer| {
            other.answer.run == answer.run && other.answer.prompt == answer.prompt
        };
        chat.pending_answers.retain(|other| !answered(other));
        let run = answer.run.clone();
        self.order(key, Order::Answer(answer));
        self.running_again(key, &run).await
    }

    // Committed first and bound to the turn it stops, so a retry after a failed write still
    // stops that turn and never a later one.
    async fn interrupt(&mut self, key: &ChatKey, request: RequestId) -> Result<(), Fault> {
        // While its run starts, a chat's turn is the prompt that run will take first.
        let starting = self.is_launching(key).then(|| self.runs.of(key)).flatten();
        let first = starting.and_then(|record| record.queue.first());
        let turn = self
            .turn(key)
            .or_else(|| first.map(|queued| queued.request.clone()));
        let chat = self.open(key).await?;
        // A retry keeps the turn its first try was bound to, whatever was interrupted since.
        let stops = &mut chat.pending_interrupts;
        if !stops.iter().any(|stop| stop.request == request) {
            stops.push(PendingInterrupt {
                request: request.clone(),
                turn,
                committed: false,
            });
        }
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        self.stop_turn(key, &request).await
    }

    // Sends the interrupt `request` committed, while the turn it stops is still running.
    async fn stop_turn(&mut self, key: &ChatKey, request: &RequestId) -> Result<(), Fault> {
        let turn = self.turn(key);
        let queued = self.runs.of(key).map(|record| record.queue.clone());
        let stops = &mut self.open(key).await?.pending_interrupts;
        let Some(at) = stops.iter().position(|stop| stop.request == *request) else {
            return Ok(());
        };
        let bound = stops.get(at).and_then(|stop| stop.turn.clone());
        let waits = queued.unwrap_or_default();
        // A turn not yet begun is stopped as it begins; one ended since, never.
        if bound
            .as_ref()
            .is_some_and(|bound| waits.iter().any(|queued| queued.request == *bound))
        {
            if let Some(stop) = stops.get_mut(at) {
                stop.committed = true;
            }
            return Ok(());
        }
        stops.retain(|stop| stop.request != *request);
        if bound.is_some_and(|bound| Some(bound) == turn) {
            self.order(key, Order::Interrupt);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::RunState;
    use weacon_testing::{TempDir, messages};

    use super::super::by_hand::{
        allow_one, hear, launched, on_ask, on_its_way, on_turn, waiting_in,
    };
    use super::*;
    use crate::chats::test_support::{
        Told, allow, asks, chat, command, key, logged_prompt, parts, prompt, prompt_naming, run,
        soon,
    };
    use crate::runs::Runs;

    #[tokio::test(start_paused = true)]
    async fn a_chat_a_newer_core_ran_takes_no_prompt_and_another_still_runs() {
        let (mut bench, mut engine) = parts();
        let dir = TempDir::new().unwrap();
        let newer = r#"{"workspace":"w-1","chat":"c-1","agent":"gemini","state":"idle"}"#;
        let journal = format!(r#"{{"issued":3,"chats":[{newer}]}}"#);
        std::fs::write(dir.join("runs.json"), journal).unwrap();
        engine.runs = Runs::load(&dir).unwrap();
        engine.reconcile().await;
        assert!(messages(&bench.records).contains(&"runs records kept unread"));

        let interrupt = command("r-0", "c-1", ChatAction::Interrupt);
        for asked in [
            prompt("r-1", "Hi"),
            allow("r-2", "c-1", "run-2", 1),
            interrupt,
        ] {
            let refused = engine.command(&key(), asked).await;
            assert_eq!(refused.unwrap_err().code().as_str(), "runs.newer_record");
        }
        assert_eq!(engine.open(&key()).await.unwrap().events.seq(), 0);

        let other = ChatKey {
            chat: chat("c-2"),
            ..key()
        };
        let next = command("r-2", "c-2", prompt("r-2", "More").action);
        engine.command(&other, next).await.unwrap();
        let started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            (started.run_as.key, started.run_as.run),
            (other, run("run-4"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_agent_a_logged_first_prompt_named_holds_even_if_it_wasnt_queued() {
        let (mut bench, mut engine) = parts();
        // The first prompt reaches the log, but the runs journal can't be saved.
        let path = engine.runs.jam();
        let first = prompt_naming("r-1", "Hi", AgentKind::Codex);
        let lost = engine.command(&key(), first.clone()).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "runs.not_saved");
        engine.runs.unjam(path);

        let other = prompt_naming("r-2", "Hi", AgentKind::Claude);
        let other = engine.command(&key(), other).await;
        assert_eq!(other.unwrap_err().code().as_str(), "chat.agent_conflict");
        engine.command(&key(), first).await.unwrap();

        let started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.run_as.agent, AgentKind::Codex);
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_sent_again_goes_on_from_where_its_first_try_stopped() {
        let (mut bench, mut engine) = parts();
        let chat_of = key();
        let agent = AgentKind::Claude;

        // Stopped after the prompt reached the log, before it was queued.
        engine
            .commit(&key(), logged_prompt("r-1", "Hi"))
            .await
            .unwrap();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        launched(&mut engine).await;
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        assert_eq!(engine.open(&key()).await.unwrap().events.seq(), 4);
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        started.told.try_recv().unwrap_err();
        assert_eq!(engine.open(&key()).await.unwrap().events.seq(), 4);
        let outcome = Outcome::Completed;
        hear(&mut engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        // Stopped after it was queued, before it was accepted.
        engine
            .commit(&key(), logged_prompt("r-2", "More"))
            .await
            .unwrap();
        let queued = Queued {
            request: prompt("r-2", "More").request,
            text: "More".to_owned(),
        };
        engine
            .runs
            .enqueue(&chat_of, agent, queued, 1)
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
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );

        let large = prompt("r-2", &"a".repeat(1 << 20));
        let refused = on.engine.command(&key(), large).await;
        on.engine
            .command(&key(), prompt("r-3", "Next"))
            .await
            .unwrap();
        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.event_too_large");
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Next".to_owned()))
        );
        assert!(on.engine.runs.of(&key()).unwrap().queue.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_that_gets_no_receipt_isnt_given_and_can_be_sent_again() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let asks = asks("run-1", 1);
        hear(&mut on.engine, asks).await.unwrap();
        let answer = PermissionAnswer {
            run: run("run-1"),
            prompt: 1,
            allow: true,
        };
        let action = ChatAction::Permission { answer };
        let allow = || command("r-2", "c-1", action.clone());
        on.engine.open(&key()).await.unwrap().events.jam();

        let refused = on.engine.command(&key(), allow()).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        on.agent.told.try_recv().unwrap_err();

        on.engine.command(&key(), allow()).await.unwrap();
        assert!(
            matches!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)) if answer.allow)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_whose_receipt_was_written_reaches_its_run_when_sent_again() {
        let mut on = on_ask().await;
        let (answers, answer) = allow_one("r-2");
        on.engine.open(&key()).await.unwrap().events.jam();
        let failed = on.engine.command(&key(), answers.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        // As when its receipt reached the disk all the same.
        let request = answers.request.clone();
        on.engine
            .commit_receipt(&key(), &request, Outcome::Completed)
            .await
            .unwrap();

        on.engine.command(&key(), answers.clone()).await.unwrap();
        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)));
        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_naming_another_run_gives_nothing_and_changes_no_run() {
        let mut on = on_ask().await;
        let (mut answers, _) = on_its_way(&mut on.engine, 1).await;
        if let ChatAction::Permission { answer } = &mut answers.action {
            answer.run = run("run-9");
        }

        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
        let log = &on.engine.open(&key()).await.unwrap().events;
        assert_eq!(
            log.runs().run().map(|status| &status.run),
            Some(&run("run-1"))
        );
        assert_eq!(waiting_in(&mut on.engine).await.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_already_in_the_log_reaches_its_run_once_and_is_logged_once() {
        let mut on = on_ask().await;
        // As when its receipt and its answer reached the disk but the write was reported failed.
        let (answers, answer) = on_its_way(&mut on.engine, 1).await;
        let event = RunEvent::PermissionAnswered {
            answer: answer.clone(),
        };
        hear(&mut on.engine, event).await.unwrap();

        on.engine.command(&key(), answers.clone()).await.unwrap();
        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)));
        assert_eq!(soon(on.agent.told.recv()).await, None);
        let log = &on.engine.open(&key()).await.unwrap().events;
        let entries = log.window(u64::MAX, 64).await.unwrap();
        let logged = entries.iter().filter(|entry| {
            matches!(&entry.event, ChatEvent::Run { update } if matches!(update.event, RunEvent::PermissionAnswered { .. }))
        });
        assert_eq!(logged.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_of_a_run_cut_off_by_a_restart_takes_no_answer() {
        let mut on = on_ask().await;
        // As after a restart: the log still shows the prompt, but its run is gone.
        on.engine.open_chats.remove(&key());

        let refused = on.engine.command(&key(), allow_one("r-2").0).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn answering_one_prompt_keeps_another_answer_still_on_its_way() {
        let mut on = on_ask().await;
        let asks = asks("run-1", 2);
        hear(&mut on.engine, asks).await.unwrap();
        // As when its receipt was written but its delivery was cut short.
        let (second, two) = on_its_way(&mut on.engine, 2).await;

        on.engine.command(&key(), allow_one("r-3").0).await.unwrap();
        on.engine.command(&key(), second).await.unwrap();

        let one = PermissionAnswer {
            prompt: 1,
            ..two.clone()
        };
        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(one)));
        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(two)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_under_another_commands_request_never_reaches_the_run() {
        let mut on = on_ask().await;
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        on.engine.command(&key(), interrupt).await.unwrap();
        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Interrupt));

        on.engine.command(&key(), allow_one("r-2").0).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_whose_receipt_was_written_stops_its_turn_when_sent_again() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        on.engine.open(&key()).await.unwrap().events.jam();
        let failed = on.engine.command(&key(), interrupt.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        // As when its receipt reached the disk all the same.
        let request = interrupt.request.clone();
        on.engine
            .commit_receipt(&key(), &request, Outcome::Completed)
            .await
            .unwrap();

        on.engine.command(&key(), interrupt.clone()).await.unwrap();
        on.engine.command(&key(), interrupt).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Interrupt));
        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_that_cant_be_committed_isnt_sent() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        on.engine.open(&key()).await.unwrap().events.jam();

        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        let refused = on.engine.command(&key(), interrupt).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_on_its_way_outlasts_another_whose_receipt_fails() {
        let mut on = on_ask().await;
        let (first, answer) = on_its_way(&mut on.engine, 1).await;
        on.engine.open(&key()).await.unwrap().events.jam();

        let other = allow("r-3", "c-1", "run-1", 1);
        let failed = on.engine.command(&key(), other.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        on.engine.command(&key(), first).await.unwrap();
        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)));

        // Even if the other's receipt landed, the prompt has its answer.
        on.engine
            .commit_receipt(&key(), &other.request, Outcome::Completed)
            .await
            .unwrap();
        on.engine.command(&key(), other).await.unwrap();
        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retried_interrupt_never_stops_a_later_turn() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        on.engine.open(&key()).await.unwrap().events.jam();
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        let failed = on.engine.command(&key(), interrupt.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");

        // The turn it was sent against ends, and the next prompt gets a turn of its own.
        let outcome = Outcome::Completed;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        on.engine
            .command(&key(), prompt("r-3", "More"))
            .await
            .unwrap();
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        on.engine.command(&key(), interrupt).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_whose_receipt_failed_outlasts_a_later_one_and_stops_no_later_turn() {
        let mut on = on_turn().await;
        let told = soon(on.agent.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        on.engine.open(&key()).await.unwrap().events.jam();
        let first = command("r-2", "c-1", ChatAction::Interrupt);
        let failed = on.engine.command(&key(), first.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");

        // Another interrupt stops the turn, and the next prompt gets one of its own.
        let second = command("r-3", "c-1", ChatAction::Interrupt);
        on.engine.command(&key(), second).await.unwrap();
        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Interrupt));
        let outcome = Outcome::Interrupted;
        hear(&mut on.engine, RunEvent::Finished { outcome })
            .await
            .unwrap();
        let next = prompt("r-4", "More");
        on.engine.command(&key(), next).await.unwrap();
        let told = soon(on.agent.told.recv()).await;
        assert_eq!(told, Some(Told::Send("More".to_owned())));

        on.engine.command(&key(), first).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
        assert_eq!(on.engine.turn(&key()), Some(prompt("r-4", "More").request));
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_given_whose_run_state_wasnt_saved_saves_it_when_sent_again() {
        let mut on = on_ask().await;
        let path = on.engine.runs.jam();
        let (answers, answer) = allow_one("r-2");

        let failed = on.engine.command(&key(), answers.clone()).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "runs.not_saved");
        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)));
        on.engine.runs.unjam(path);
        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
        let state = on.engine.runs.of(&key()).unwrap().state;
        assert_eq!(state, RunState::Running);
    }
}
