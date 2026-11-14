//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, Outcome, PermissionAnswer, RepoId, RequestId,
    RunEvent, RunState, RunUpdate, code,
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
        let seen = self.open(key).await?.log.seen(&request, &action).await?;
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
        match action {
            ChatAction::Prompt { text, repo, .. } => {
                if seen != Seen::Pending {
                    self.log_prompt(key, &request, &text, repo).await?;
                }
                self.prompt(key, request, text).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            ChatAction::Newer => Err(unsupported()),
        }
    }

    // A prompt is in the log before it is queued, and queued before it is accepted. Whichever
    // step a first try stopped at, sending it again takes it from there.
    async fn log_prompt(
        &mut self,
        key: &ChatKey,
        request: &RequestId,
        text: &str,
        repo: Option<RepoId>,
    ) -> Result<(), Fault> {
        if text.trim().is_empty() {
            return Err(Fault::new(
                code!("chat.empty_prompt"),
                "The prompt is empty",
            ));
        }
        let prompt = ChatEvent::Prompt {
            request: request.clone(),
            text: text.to_owned(),
            repo,
        };
        self.commit(key, prompt).await
    }

    async fn prompt(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        text: String,
    ) -> Result<(), Fault> {
        let queued = Queued {
            request: request.clone(),
            text,
        };
        // Until a prompt can name its agent, a new chat runs Claude.
        let (agent, now) = (AgentKind::Claude, (self.host.now)());
        self.runs.enqueue(key, agent, queued, now).await?;
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
        if !live || !chat.log.runs().waits(&answer.run, answer.prompt) {
            return Err(Fault::new(
                code!("agent.no_such_prompt"),
                "That permission prompt isn't waiting",
            ));
        }
        let answers = &mut chat.pending_answers;
        answers.retain(|other| other.answer.prompt != answer.prompt);
        answers.push(PendingAnswer {
            request: request.clone(),
            answer: answer.clone(),
        });
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
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
        let Some(at) = chat
            .pending_answers
            .iter()
            .position(pending)
            .filter(|_| live)
        else {
            return Ok(());
        };
        if chat.log.runs().waits(&answer.run, answer.prompt) {
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
        chat.pending_answers.remove(at);
        let run = answer.run.clone();
        let resumed = !chat.log.runs().waiting.iter().any(|waits| waits.run == run);
        self.order(key, Order::Answer(answer));
        if resumed {
            self.set_state(key, &run, RunState::Running).await?;
        }
        Ok(())
    }

    // Committed first and bound to the turn it stops, so a retry after a failed write still
    // stops that turn and never a later one.
    async fn interrupt(&mut self, key: &ChatKey, request: RequestId) -> Result<(), Fault> {
        let stops = self.turn(key).map(|turn| PendingInterrupt {
            request: request.clone(),
            turn,
        });
        self.open(key).await?.pending_interrupt = stops;
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        self.stop_turn(key, &request).await
    }

    // Sends the interrupt `request` committed, while the turn it stops is still running.
    async fn stop_turn(&mut self, key: &ChatKey, request: &RequestId) -> Result<(), Fault> {
        let Some(turn) = self.turn(key) else {
            return Ok(());
        };
        let stops = Some(PendingInterrupt {
            request: request.clone(),
            turn,
        });
        let chat = self.open(key).await?;
        if chat.pending_interrupt != stops {
            return Ok(());
        }
        chat.pending_interrupt = None;
        self.order(key, Order::Interrupt);
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use weacon_protocol::{ChatAction, ChatCommand};

    use super::super::PendingAnswer;
    use super::super::by_hand::{allow_one, hear, on_ask, on_its_way, on_turn, waiting_in};
    use super::*;
    use crate::chats::test_support::{
        Told, asks, command, key, logged_prompt, parts, prompt, run, soon,
    };
    use crate::runs::Queued;

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
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        assert_eq!(engine.open(&key()).await.unwrap().log.seq(), 4);
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        started.told.try_recv().unwrap_err();
        assert_eq!(engine.open(&key()).await.unwrap().log.seq(), 4);
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
        on.engine.open(&key()).await.unwrap().log.jam();

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
        on.engine.open(&key()).await.unwrap().log.jam();
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
        let mut answers = on_its_way(&mut on.engine).await;
        if let ChatAction::Permission { answer } = &mut answers.action {
            answer.run = run("run-9");
        }

        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, None);
        let runs = &on.engine.open(&key()).await.unwrap().log.runs().runs;
        assert_eq!(runs[0].run, run("run-1"));
        assert_eq!(waiting_in(&mut on.engine).await.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_already_in_the_log_reaches_its_run_once_and_is_logged_once() {
        let mut on = on_ask().await;
        let (answers, answer) = allow_one("r-2");
        let answered = PendingAnswer {
            request: answers.request.clone(),
            answer: PermissionAnswer {
                run: run("run-1"),
                prompt: 1,
                allow: true,
            },
        };
        // As when its receipt and its answer reached the disk but the write was reported failed.
        on.engine
            .open(&key())
            .await
            .unwrap()
            .pending_answers
            .push(answered);
        let receipt = Outcome::Completed;
        on.engine
            .commit_receipt(&key(), &answers.request, receipt)
            .await
            .unwrap();
        let event = RunEvent::PermissionAnswered {
            answer: answer.clone(),
        };
        hear(&mut on.engine, event).await.unwrap();

        on.engine.command(&key(), answers.clone()).await.unwrap();
        on.engine.command(&key(), answers).await.unwrap();

        assert_eq!(soon(on.agent.told.recv()).await, Some(Told::Answer(answer)));
        assert_eq!(soon(on.agent.told.recv()).await, None);
        let log = &on.engine.open(&key()).await.unwrap().log;
        let entries = log.window(u64::MAX, 64).await.unwrap();
        let logged = entries.iter().filter(|entry| {
            matches!(&entry.event, ChatEvent::Run { update } if matches!(update.event, RunEvent::PermissionAnswered { .. }))
        });
        assert_eq!(logged.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_of_a_run_cut_off_by_a_restart_takes_no_answer() {
        let mut on = on_ask().await;
        // As after a restart: the log still shows the prompt, but its run is gone.
        on.engine.chats.remove(&key());

        let refused = on.engine.command(&key(), allow_one("r-2").0).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn answering_one_prompt_keeps_another_answer_still_on_its_way() {
        let mut on = on_ask().await;
        let asks = asks("run-1", 2);
        hear(&mut on.engine, asks).await.unwrap();
        let (second, given) = allow_one("r-2");
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
        let pending = PendingAnswer {
            request: second.request.clone(),
            answer: PermissionAnswer {
                run: run("run-1"),
                prompt: 2,
                allow: true,
            },
        };
        on.engine
            .open(&key())
            .await
            .unwrap()
            .pending_answers
            .push(pending);
        let receipt = Outcome::Completed;
        on.engine
            .commit_receipt(&key(), &second.request, receipt)
            .await
            .unwrap();

        on.engine.command(&key(), allow_one("r-3").0).await.unwrap();
        on.engine.command(&key(), second).await.unwrap();

        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Answer(given.clone()))
        );
        let two = PermissionAnswer { prompt: 2, ..given };
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
        on.engine.open(&key()).await.unwrap().log.jam();
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
        on.engine.open(&key()).await.unwrap().log.jam();

        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        let refused = on.engine.command(&key(), interrupt).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        assert_eq!(soon(on.agent.told.recv()).await, None);
    }
}
