//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, Outcome, PermissionAnswer, RepoId, RequestId,
    RunEvent, RunUpdate, code,
};

use super::agent::{Agent, Order};
use super::launch::not_started;
use super::{ChatKey, Engine, PendingInterrupt, chat_about, unsupported};
use crate::git::state::Stores;
use crate::runs::Queued;

impl<A: Agent> Engine<A> {
    pub(super) async fn command(
        &mut self,
        key: &ChatKey,
        command: ChatCommand,
    ) -> Result<(), Fault> {
        let request = command.request;
        let acted = self.act(key, request.clone(), command.action).await;
        let Err(fault) = acted else {
            return Ok(());
        };
        // Taken once its receipt is in the log, whatever failed after.
        if !self.open(key).await?.log.has_receipt(&request) {
            return Err(fault);
        }
        let fields = [&chat_about(key)[..], &[("err", Field::error(&fault))]].concat();
        self.log.warn("chat command cut short", &fields);
        Ok(())
    }

    async fn act(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        action: ChatAction,
    ) -> Result<(), Fault> {
        // Before the log, so a chat a newer core runs gets nothing from this one.
        self.runs.refuse_newer(key)?;
        match action {
            ChatAction::Prompt {
                text,
                repo,
                agent: named,
                task,
                ..
            } => {
                let agent = self.agent_of(key, named).await?;
                self.check_repo(key, repo.as_ref()).await?;
                if text.trim().is_empty() {
                    return Err(Fault::new(
                        code!("chat.empty_prompt"),
                        "The prompt is empty",
                    ));
                }
                self.has_room(key)?;
                let prompt = ChatEvent::Prompt {
                    request: request.clone(),
                    text: text.clone(),
                    repo,
                    agent: named,
                    task,
                };
                self.commit(key, prompt).await?;
                self.prompt(key, request, text, agent).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            // Taken before the log, by `command_from`.
            ChatAction::Hold | ChatAction::Release | ChatAction::Newer => Err(unsupported()),
        }
    }

    // The chat's agent is the one its first logged prompt named; a later prompt can't change it.
    async fn agent_of(
        &mut self,
        key: &ChatKey,
        named: Option<AgentKind>,
    ) -> Result<AgentKind, Fault> {
        let chosen = self.open(key).await?.log.agent();
        match (named, chosen) {
            (Some(AgentKind::Newer), _) => Err(unsupported()),
            (Some(named), Some(chosen)) if named != chosen => Err(Fault::new(
                code!("chat.agent_conflict"),
                "The chat already runs another agent",
            )),
            (named, chosen) => Ok(chosen.or(named).unwrap_or(AgentKind::Claude)),
        }
    }

    // The chat's repository is the one its first logged prompt named: that one has to be
    // registered in the workspace, and a later prompt can't name another.
    async fn check_repo(&mut self, key: &ChatKey, named: Option<&RepoId>) -> Result<(), Fault> {
        let events = &self.open(key).await?.log;
        let Some(named) = named else {
            return Ok(());
        };
        if events.agent().is_some() {
            return match events.repo() {
                Some(chosen) if chosen == named => Ok(()),
                _ => Err(Fault::new(
                    code!("chat.repo_conflict"),
                    "The chat already works in another repository, or in none",
                )),
            };
        }
        let stores = Stores::new(&self.workspaces, &key.workspace);
        let named = named.clone();
        tokio::task::spawn_blocking(move || stores.repo(&named).map(drop))
            .await
            .unwrap_or_else(|_| Err(not_started()))
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
        if !live || !chat.log.runs().waits(&answer.run, answer.prompt) {
            return Err(Fault::new(
                code!("agent.no_such_prompt"),
                "That permission prompt isn't waiting",
            ));
        }
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        // In the log before the agent is told, so a crash between the two leaves the truth.
        let update = RunUpdate {
            run: answer.run.clone(),
            event: RunEvent::PermissionAnswered {
                answer: answer.clone(),
            },
        };
        self.commit(key, ChatEvent::Run { update }).await?;
        let run = answer.run.clone();
        self.order(key, Order::Answer(answer));
        self.running_again(key, &run).await
    }

    // Bound to the turn it stops, so it never stops a later one.
    async fn interrupt(&mut self, key: &ChatKey, request: RequestId) -> Result<(), Fault> {
        // While its run starts, a chat's turn is the prompt that run will take first.
        let starting = self.is_launching(key).then(|| self.runs.of(key)).flatten();
        let first = starting.and_then(|record| record.queue.first());
        let turn = self
            .turn(key)
            .or_else(|| first.map(|queued| queued.request.clone()));
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        self.open(key)
            .await?
            .pending_interrupts
            .push(PendingInterrupt {
                request: request.clone(),
                turn,
                committed: false,
            });
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
    use weacon_testing::{TempDir, messages};

    use super::super::by_hand::{allow_one, hear, launched, on_ask, on_turn};
    use super::*;
    use crate::chats::test_support::{
        Told, allow, asks, chat, command, key, parts, prompt, prompt_in, prompt_naming, run, soon,
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
        assert_eq!(engine.open(&key()).await.unwrap().log.seq(), 0);

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

    #[tokio::test]
    async fn the_repository_a_first_prompt_named_is_the_chats_for_good() {
        let (mut bench, mut engine) = parts();
        let site = bench.register("site");
        let other = bench.register("other");

        engine
            .command(&key(), prompt_in("r-1", "Hi", &site))
            .await
            .unwrap();
        launched(&mut engine).await;

        let started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.run_as.repo, Some(site.clone()));
        let moved = engine.command(&key(), prompt_in("r-2", "Go", &other)).await;
        assert_eq!(moved.unwrap_err().code().as_str(), "chat.repo_conflict");
        engine
            .command(&key(), prompt_in("r-3", "Go", &site))
            .await
            .unwrap();
        engine.command(&key(), prompt("r-4", "Go")).await.unwrap();
    }

    fn with_task(mut asked: ChatCommand, task: weacon_protocol::ChatTask) -> ChatCommand {
        if let ChatAction::Prompt { task: named, .. } = &mut asked.action {
            *named = Some(task);
        }
        asked
    }

    #[tokio::test]
    async fn a_run_starts_with_the_task_its_chats_first_prompt_named() {
        let (mut bench, mut engine) = parts();
        let task = |id: &str| weacon_protocol::ChatTask {
            id: id.parse().unwrap(),
            title: "Speed up the build".to_owned(),
        };
        let for_task = |request: &str, id: &str| with_task(prompt(request, "Hi"), task(id));

        engine
            .command(&key(), for_task("r-1", "t-1"))
            .await
            .unwrap();
        launched(&mut engine).await;
        engine
            .command(&key(), for_task("r-2", "t-2"))
            .await
            .unwrap();

        let started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.run_as.task, Some(task("t-1")));
        let log = &engine.open(&key()).await.unwrap().log;
        assert_eq!(log.task(), Some(&task("t-1")));
    }

    #[tokio::test]
    async fn a_first_prompt_naming_an_unregistered_repository_is_refused_unlogged() {
        let (bench, mut engine) = parts();
        let unknown = RepoId::try_from("typo".to_owned()).unwrap();

        let refused = engine
            .command(&key(), prompt_in("r-1", "Hi", &unknown))
            .await;

        assert_eq!(refused.unwrap_err().code().as_str(), "repo.unknown");
        assert_eq!(engine.open(&key()).await.unwrap().log.seq(), 0);
        let later = bench.register("site");
        engine.command(&key(), prompt("r-2", "Hi")).await.unwrap();
        let named = engine.command(&key(), prompt_in("r-3", "Go", &later)).await;
        assert_eq!(named.unwrap_err().code().as_str(), "chat.repo_conflict");
    }

    #[tokio::test(start_paused = true)]
    async fn the_agent_a_logged_first_prompt_named_holds_even_if_it_wasnt_queued() {
        let (mut bench, mut engine) = parts();
        // The first prompt reaches the log, but the runs journal can't be saved.
        let path = engine.runs.jam();
        let first = prompt_naming("r-1", "Hi", AgentKind::Codex);
        let lost = engine.command(&key(), first).await;
        assert_eq!(lost.unwrap_err().code().as_str(), "runs.not_saved");
        engine.runs.unjam(path);

        let other = prompt_naming("r-2", "Hi", AgentKind::Claude);
        let conflict = engine.command(&key(), other).await.unwrap_err();
        assert_eq!(conflict.code().as_str(), "chat.agent_conflict");
        let next = prompt_naming("r-3", "Hi", AgentKind::Codex);
        engine.command(&key(), next).await.unwrap();

        let started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.run_as.agent, AgentKind::Codex);
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
    async fn a_permission_of_a_run_cut_off_by_a_restart_takes_no_answer() {
        let mut on = on_ask().await;
        // As after a restart: the log still shows the prompt, but its run is gone.
        on.engine.open_chats.remove(&key());

        let refused = on.engine.command(&key(), allow_one("r-2").0).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "agent.no_such_prompt");
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

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_whose_receipt_failed_stops_no_later_turn() {
        let mut on = on_turn().await;
        let told = soon(on.agent.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        on.engine.open(&key()).await.unwrap().log.jam();
        let first = command("r-2", "c-1", ChatAction::Interrupt);
        let failed = on.engine.command(&key(), first).await;
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

        assert_eq!(soon(on.agent.told.recv()).await, None);
        assert_eq!(on.engine.turn(&key()), Some(prompt("r-4", "More").request));
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_whose_receipt_failed_while_the_agent_started_stops_nothing() {
        let (mut bench, mut engine) = parts();
        let release = bench.hold_next_start();
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        engine.open(&key()).await.unwrap().log.jam();
        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);

        let failed = engine.command(&key(), interrupt).await;
        assert_eq!(failed.unwrap_err().code().as_str(), "chat.log_not_written");
        drop(release);
        launched(&mut engine).await;

        let mut started = soon(bench.started.recv()).await.unwrap();
        let told = soon(started.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        assert_eq!(soon(started.told.recv()).await, None);
    }
}
