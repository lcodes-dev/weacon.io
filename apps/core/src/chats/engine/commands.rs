//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, MemberId, Outcome, PermissionAnswer, RepoId,
    RequestId, RunEvent, RunUpdate, code,
};

use super::agent::{Agent, Order};
use super::launch::not_started;
use super::{ChatKey, Engine, PendingInterrupt, chat_about, unsupported};
use crate::chats::quoted;
use crate::git::state::Stores;
use crate::runs::Queued;

impl<A: Agent> Engine<A> {
    pub(super) async fn command(
        &mut self,
        key: &ChatKey,
        command: ChatCommand,
    ) -> Result<(), Fault> {
        if matches!(&command.action, ChatAction::Prompt { pick: Some(_), .. })
            && command.by.is_some()
        {
            return Err(super::picked::local_only());
        }
        let request = command.request;
        let author = command.by.map(|by| by.member);
        let asked = (request.clone(), author);
        let acted = self.act(key, asked, command.action).await;
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
        (request, author): (RequestId, Option<MemberId>),
        action: ChatAction,
    ) -> Result<(), Fault> {
        // Before the log, so a chat a newer core runs gets nothing from this one.
        self.runs.refuse_newer(key)?;
        match action {
            ChatAction::Prompt {
                text,
                quotes,
                repo,
                agent: named,
                pick,
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
                // The log keeps the person's own words; the agent also gets the quotes.
                let told = quoted::prompt(&text, &quotes);
                let prompt = ChatEvent::Prompt {
                    pick: self.pick_note(pick.as_ref()).await?,
                    request: request.clone(),
                    text,
                    author,
                    repo,
                    agent: named,
                    task,
                };
                self.commit(key, prompt).await?;
                let queued = Queued {
                    request,
                    text: told,
                    pick,
                };
                self.prompt(key, queued, agent).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            // A hold is taken before the log, by `command_from`, and only the server takes a
            // message to a chat's people.
            ChatAction::Say { .. } | ChatAction::Hold | ChatAction::Release | ChatAction::Newer => {
                Err(unsupported())
            }
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
        queued: Queued,
        agent: AgentKind,
    ) -> Result<(), Fault> {
        let request = queued.request.clone();
        self.runs.enqueue(key, agent, queued, self.now()).await?;
        if let Err(fault) = self.commit_receipt(key, &request, Outcome::Accepted).await {
            // Refused, so it never runs.
            let taken_back = |record: &mut crate::runs::Record| {
                record.queue.retain(|queued| queued.request != request);
            };
            self.runs.update(key, taken_back).await?;
            return Err(fault);
        }
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
        let logged = self.commit(key, ChatEvent::Run { update }).await;
        let run = answer.run.clone();
        // Its receipt is in, so the agent is told even when the answer's own entry failed.
        self.order(key, Order::Answer(answer));
        logged?;
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
    async fn an_agent_gets_the_others_words_as_quotes_and_the_log_keeps_its_owners_alone_under_their_name()
     {
        let (mut bench, mut engine) = parts();
        let mut asked = prompt("r-1", "MARKER-OWN");
        if let ChatAction::Prompt { quotes, .. } = &mut asked.action {
            quotes.push(weacon_protocol::Quote {
                name: "Grace".to_owned(),
                agent: None,
                text: "MARKER-QUOTE".to_owned(),
            });
        }
        let ada: MemberId = "m-1".parse().unwrap();
        asked.by = Some(weacon_protocol::Asker {
            account: "a-1".parse().unwrap(),
            member: ada.clone(),
        });

        engine.command(&key(), asked).await.unwrap();
        launched(&mut engine).await;

        let mut agent = soon(bench.started.recv()).await.unwrap();
        let told = quoted::prompt("MARKER-OWN", &[quote("Grace", "MARKER-QUOTE")]);
        assert!(told.contains("Grace wrote:\nMARKER-QUOTE"), "{told}");
        assert_eq!(soon(agent.told.recv()).await, Some(Told::Send(told)));
        let log = &engine.open(&key()).await.unwrap().log;
        let entries = log.window(u64::MAX, 9).await.unwrap();
        let events: Vec<_> = entries.into_iter().map(|entry| entry.event).collect();
        let kept = ChatEvent::Prompt {
            request: "r-1".parse().unwrap(),
            text: "MARKER-OWN".to_owned(),
            pick: None,
            author: Some(ada.clone()),
            repo: None,
            agent: None,
            task: None,
        };
        assert_eq!(events.first(), Some(&kept));
        // Every status of the run names whose agent it is.
        let named = weacon_protocol::AgentRef {
            kind: AgentKind::Claude,
            owner: weacon_protocol::AgentOwner::Member { member: ada },
            machine: None,
        };
        let statuses: Vec<_> = events.iter().filter_map(status_of).collect();
        assert!(!statuses.is_empty());
        let named = Some(named);
        assert!(statuses.iter().all(|status| status.agent == named));
        let logged = serde_json::to_string(&*bench.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_chats_agent_takes_each_members_prompt_and_logs_who_wrote_it() {
        let (_bench, mut engine) = parts();
        let from = |request: &str, member: &str| {
            let mut asked = prompt(request, "Go");
            asked.by = Some(weacon_protocol::Asker {
                account: "a-9".parse().unwrap(),
                member: member.parse().unwrap(),
            });
            asked
        };

        engine.command(&key(), from("r-1", "m-1")).await.unwrap();
        engine.command(&key(), from("r-2", "m-2")).await.unwrap();

        let log = &engine.open(&key()).await.unwrap().log;
        let entries = log.window(u64::MAX, 9).await.unwrap();
        let authors: Vec<_> = entries.iter().filter_map(author_of).collect();
        let authors: Vec<_> = authors.iter().map(MemberId::as_str).collect();
        assert_eq!(authors, ["m-1", "m-2"]);
    }

    fn author_of(entry: &weacon_protocol::ChatEntry) -> Option<MemberId> {
        let ChatEvent::Prompt { author, .. } = &entry.event else {
            return None;
        };
        author.clone()
    }

    fn status_of(event: &ChatEvent) -> Option<&weacon_protocol::RunStatus> {
        let ChatEvent::RunStatus { status } = event else {
            return None;
        };
        Some(status)
    }

    fn quote(name: &str, text: &str) -> weacon_protocol::Quote {
        weacon_protocol::Quote {
            name: name.to_owned(),
            agent: None,
            text: text.to_owned(),
        }
    }

    #[tokio::test]
    async fn a_message_to_a_chats_people_is_never_a_cores_to_take() {
        let (_bench, mut engine) = parts();
        let say = ChatAction::Say {
            text: "Morning".to_owned(),
            task: None,
        };

        let refused = engine.command(&key(), command("r-1", "c-1", say)).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
        assert_eq!(engine.open(&key()).await.unwrap().log.seq(), 0);
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
    async fn an_answer_whose_receipt_is_in_is_given_though_its_entry_fails() {
        let mut on = on_turn().await;
        assert_eq!(
            soon(on.agent.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        hear(&mut on.engine, asks("run-1", 1)).await.unwrap();
        let answer = PermissionAnswer {
            run: run("run-1"),
            prompt: 1,
            allow: true,
        };
        let allow = command("r-2", "c-1", ChatAction::Permission { answer });
        on.engine.open(&key()).await.unwrap().log.jam_after(1);

        on.engine.command(&key(), allow).await.unwrap();
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
    async fn a_prompt_whose_receipt_fails_leaves_the_queue() {
        let (_bench, mut engine) = parts();
        engine.open(&key()).await.unwrap().log.jam();

        let queued = Queued {
            request: crate::chats::test_support::request("r-1"),
            text: "Hi".to_owned(),
            pick: None,
        };
        let refused = engine.prompt(&key(), queued, AgentKind::Claude).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.log_not_written");
        let queue = engine.runs.of(&key()).map(|record| record.queue.len());
        assert_eq!(queue, Some(0));
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
