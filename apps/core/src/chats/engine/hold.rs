//! A hold: a local client carrying a chat's session on by hand, while no turn starts for it.
//! It lives on the open chat only, and ends with its holder's connection.

use weacon_observability::Fault;
use weacon_protocol::{ChatAction, ChatCommand, code};

use super::agent::Agent;
use super::rest::chat_fields;
use super::{ChatKey, Engine, chat_about};
use crate::chats::feed::Client;

/// How many prompts a held chat queues.
const HELD_QUEUE: usize = 16;

impl<A: Agent> Engine<A> {
    // A hold and a release belong to the client that sent them, and never reach the log.
    pub(super) async fn command_from(
        &mut self,
        key: &ChatKey,
        command: ChatCommand,
        client: &Client,
    ) -> Result<(), Fault> {
        match command.action {
            ChatAction::Hold => self.hold(key, client).await,
            ChatAction::Release => self.release(key, client).await,
            ChatAction::Prompt { .. }
            | ChatAction::Permission { .. }
            | ChatAction::Interrupt
            | ChatAction::Newer => self.command(key, command).await,
        }
    }

    async fn hold(&mut self, key: &ChatKey, client: &Client) -> Result<(), Fault> {
        let chat = self.open(key).await?;
        let holder = chat.held.as_ref().filter(|holder| !holder.is_gone());
        if holder.is_some_and(|holder| holder.is(client)) {
            return Ok(());
        }
        // A live run, a starting one or a session still to save would race the hand's agent.
        let busy = chat.orders.is_some() || chat.is_starting();
        if holder.is_some() || busy || chat.pending_session.is_some() {
            return Err(Fault::new(
                code!("chat.busy"),
                "The chat's agent is running, or someone else holds it",
            ));
        }
        chat.held = Some(client.clone());
        self.log.info("chat held", &chat_about(key));
        Ok(())
    }

    async fn release(&mut self, key: &ChatKey, client: &Client) -> Result<(), Fault> {
        let chat = self.open(key).await?;
        if !chat.held.as_ref().is_some_and(|holder| holder.is(client)) {
            return Err(Fault::new(
                code!("chat.not_holder"),
                "This connection doesn't hold the chat",
            ));
        }
        chat.held = None;
        self.log.info("chat released", &chat_about(key));
        self.advance(key).await
    }

    pub(super) fn is_held(&self, key: &ChatKey) -> bool {
        self.open_chats
            .get(key)
            .and_then(|chat| chat.held.as_ref())
            .is_some_and(|holder| !holder.is_gone())
    }

    // Refuses a prompt a held chat has no room to queue.
    pub(super) fn has_room(&self, key: &ChatKey) -> Result<(), Fault> {
        if !self.is_held(key) {
            return Ok(());
        }
        let queued = self.runs.of(key).map_or(0, |record| record.queue.len());
        if queued >= HELD_QUEUE {
            return Err(Fault::new(
                code!("chat.held"),
                "The chat is carried on by hand and can't queue more prompts",
            ));
        }
        Ok(())
    }

    // Lets go of clients that hung up, and of the chats only they were using; a chat one of
    // them held takes its queued prompts on.
    pub(super) async fn close_unused(&mut self) {
        let released: Vec<ChatKey> = self
            .open_chats
            .iter_mut()
            .filter_map(|(key, chat)| chat.forget_gone().then(|| key.clone()))
            .collect();
        for key in released {
            self.log.info("chat released", &chat_about(&key));
            let advanced = self.advance(&key).await;
            if let Some(fault) = self.unlogged(&key, None, advanced).await {
                self.log
                    .warn("chat not picked up", &chat_fields(&key, &fault));
            }
        }
        self.open_chats.retain(|_, chat| !chat.is_unused());
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use weacon_protocol::{AgentKind, ChatAnswerKind, ChatQueryKind, Message, Outcome, RunEvent};

    use super::super::IDLE_CHECK;
    use super::*;
    use crate::chats::test_support::{
        Bench, Told, bench, client, command, key, parts, prompt, pushed, query, request, session,
        skip, soon,
    };

    fn hold(request: &str) -> ChatCommand {
        command(request, "c-1", ChatAction::Hold)
    }

    fn release(request: &str) -> ChatCommand {
        command(request, "c-1", ChatAction::Release)
    }

    fn code_of(refused: Result<(), Fault>) -> String {
        refused.unwrap_err().code().as_str().to_owned()
    }

    async fn seq(bench: &Bench) -> u64 {
        let (anyone, _pushes) = client(1);
        let snapshot = query("c-1", ChatQueryKind::Snapshot);
        let Ok(Some(ChatAnswerKind::Snapshot { seq, .. })) =
            bench.chats.query(snapshot, &anyone).await
        else {
            panic!("no snapshot");
        };
        seq
    }

    // A bench whose chat `c-1` is held, with a prompt queued, and its holder.
    async fn held_with_a_prompt() -> (Bench, Client, mpsc::Receiver<Message>) {
        let bench = bench();
        let (holder, pushes) = client(8);
        bench.chats.command(hold("h-1"), &holder).await.unwrap();
        bench.command(prompt("r-1", "Hi")).await.unwrap();
        (bench, holder, pushes)
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_is_refused_while_a_turn_runs_or_a_run_starts() {
        let mut on_turn = bench();
        on_turn.command(prompt("r-1", "Hi")).await.unwrap();
        let _agent = soon(on_turn.started.recv()).await.unwrap();
        let (holder, _pushes) = client(1);
        let refused = on_turn.chats.command(hold("h-1"), &holder).await;
        assert_eq!(code_of(refused), "chat.busy");

        let starting = bench();
        let release = starting.hold_next_start();
        starting.command(prompt("r-1", "Hi")).await.unwrap();
        let refused = starting.chats.command(hold("h-1"), &holder).await;
        assert_eq!(code_of(refused), "chat.busy");
        drop(release);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_is_refused_while_the_chats_agent_is_up_between_turns() {
        let mut bench = bench();
        let (watcher, mut pushes) = client(16);
        let subscribe = query("c-1", ChatQueryKind::Subscribe { after: 0 });
        bench.chats.query(subscribe, &watcher).await.unwrap();
        bench.command(prompt("r-1", "Hi")).await.unwrap();
        let agent = soon(bench.started.recv()).await.unwrap();
        let outcome = Outcome::Completed;
        agent.say.send(RunEvent::Finished { outcome }).unwrap();
        skip(&mut pushes, 6).await;
        assert_eq!(pushed(&mut pushes, 1).await, ["7 run-1 is Idle"]);

        let (holder, _pushes) = client(1);
        let refused = bench.chats.command(hold("h-1"), &holder).await;

        assert_eq!(code_of(refused), "chat.busy");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_is_refused_while_a_session_waits_to_be_saved() {
        let (_bench, mut engine) = parts();
        engine.open(&key()).await.unwrap().pending_session = Some(session());
        let (holder, _pushes) = client(1);

        let refused = engine.command_from(&key(), hold("h-1"), &holder).await;

        assert_eq!(code_of(refused), "chat.busy");
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_the_journal_kept_with_no_run_left_doesnt_stop_a_hold() {
        let (_bench, mut engine) = parts();
        engine
            .runs
            .begin(&key(), AgentKind::Claude, 100)
            .await
            .unwrap();
        let turn = |record: &mut crate::runs::Record| record.turn = Some(request("r-9"));
        engine.runs.update(&key(), turn).await.unwrap();
        let (holder, _pushes) = client(1);

        engine
            .command_from(&key(), hold("h-1"), &holder)
            .await
            .unwrap();

        assert!(engine.is_held(&key()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_holder_that_hung_up_gives_way_before_its_chat_is_closed() {
        let (_bench, mut engine) = parts();
        let ((first, gone), (second, _pushes)) = (client(1), client(1));
        engine
            .command_from(&key(), hold("h-1"), &first)
            .await
            .unwrap();
        drop(gone);

        engine
            .command_from(&key(), hold("h-2"), &second)
            .await
            .unwrap();

        let refused = engine.command_from(&key(), release("h-3"), &first).await;
        assert_eq!(code_of(refused), "chat.not_holder");
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_no_one_holds_queues_more_than_a_held_one() {
        let bench = bench();
        bench.chats.disk_full(true);

        for at in 0..=HELD_QUEUE {
            let asked = prompt(&format!("r-{at}"), "More");
            bench.command(asked).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_has_one_holder_who_may_hold_it_again() {
        let bench = bench();
        let ((first, _one), (second, _two)) = (client(1), client(1));

        bench.chats.command(hold("h-1"), &first).await.unwrap();
        bench.chats.command(hold("h-2"), &first).await.unwrap();
        let refused = bench.chats.command(hold("h-3"), &second).await;

        assert_eq!(code_of(refused), "chat.busy");
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_chat_queues_prompts_and_takes_them_on_once_released() {
        let (mut bench, holder, _pushes) = held_with_a_prompt().await;
        let (watcher, mut pushes) = client(8);
        let subscribe = query("c-1", ChatQueryKind::Subscribe { after: 0 });
        bench.chats.query(subscribe, &watcher).await.unwrap();
        assert_eq!(
            pushed(&mut pushes, 2).await,
            ["1 prompt r-1 Hi", "2 receipt r-1 accepted"]
        );
        assert!(soon(bench.started.recv()).await.is_none());

        bench.chats.command(release("h-2"), &holder).await.unwrap();

        let mut started = soon(bench.started.recv()).await.unwrap();
        let told = soon(started.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        let running = pushed(&mut pushes, 2).await;
        assert_eq!(running, ["3 run-1 is Starting", "4 run-1 is Running"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_chat_refuses_its_seventeenth_prompt_until_it_is_released() {
        let (bench, holder, _pushes) = held_with_a_prompt().await;
        for at in 2..=HELD_QUEUE {
            let asked = prompt(&format!("r-{at}"), "More");
            bench.command(asked).await.unwrap();
        }
        let (watcher, mut pushes) = client(8);
        let subscribe = query("c-1", ChatQueryKind::Subscribe { after: 32 });
        bench.chats.query(subscribe, &watcher).await.unwrap();

        let refused = bench.command(prompt("r-17", "One too many")).await;
        assert_eq!(code_of(refused), "chat.held");
        assert_eq!(seq(&bench).await, 32);
        bench.chats.command(release("h-2"), &holder).await.unwrap();
        bench.command(prompt("r-17", "One too many")).await.unwrap();

        let logged = pushed(&mut pushes, 4).await;
        let has = |end: &str| logged.iter().any(|entry| entry.ends_with(end));
        assert!(
            has("prompt r-17 One too many") && has("receipt r-17 accepted"),
            "{logged:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_holder_releases_a_chat() {
        let (mut bench, _holder, _pushes) = held_with_a_prompt().await;
        let (other, _pushed) = client(1);

        let refused = bench.chats.command(release("h-2"), &other).await;

        assert_eq!(code_of(refused), "chat.not_holder");
        assert!(soon(bench.started.recv()).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_holder_that_hangs_up_lets_the_chat_go_on_once_the_daemon_says_so() {
        let (mut bench, _holder, pushes) = held_with_a_prompt().await;
        drop(pushes);
        assert!(soon(bench.started.recv()).await.is_none());

        bench.chats.left().await;

        let mut started = soon(bench.started.recv()).await.unwrap();
        let told = soon(started.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
    }

    #[tokio::test(start_paused = true)]
    async fn the_idle_check_lets_go_of_a_holder_whose_hang_up_went_unreported() {
        let (mut bench, _holder, pushes) = held_with_a_prompt().await;
        drop(pushes);
        assert!(soon(bench.started.recv()).await.is_none());

        // Moved on, not slept: a start under way would hold a paused clock back.
        tokio::time::advance(IDLE_CHECK).await;

        assert!(soon(bench.started.recv()).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_is_never_logged_even_under_another_commands_request() {
        let bench = bench();
        let (holder, _pushes) = client(1);
        let interrupt = command("r-1", "c-1", ChatAction::Interrupt);
        bench.command(interrupt).await.unwrap();

        bench.chats.command(hold("r-1"), &holder).await.unwrap();
        bench.chats.command(release("r-1"), &holder).await.unwrap();

        assert_eq!(seq(&bench).await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_chat_stays_open_until_its_holder_is_gone() {
        let (_bench, mut engine) = parts();
        let (holder, pushes) = client(1);
        engine
            .command_from(&key(), hold("h-1"), &holder)
            .await
            .unwrap();

        engine.close_if_unused(&key());
        engine.close_unused().await;
        assert!(engine.open_chats.contains_key(&key()));
        drop(pushes);
        engine.close_unused().await;

        assert!(!engine.open_chats.contains_key(&key()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_whose_holder_hung_up_takes_more_prompts_before_it_is_closed() {
        let (_bench, mut engine) = parts();
        let (holder, gone) = client(1);
        engine
            .command_from(&key(), hold("h-1"), &holder)
            .await
            .unwrap();
        for at in 1..=HELD_QUEUE {
            let asked = prompt(&format!("r-{at}"), "More");
            engine.command(&key(), asked).await.unwrap();
        }
        drop(gone);

        engine
            .command(&key(), prompt("r-17", "Next"))
            .await
            .unwrap();

        let log = &engine.open(&key()).await.unwrap().log;
        assert!(!log.has_ended(&request("r-17")));
        assert!(engine.is_launching(&key()));
    }
}
