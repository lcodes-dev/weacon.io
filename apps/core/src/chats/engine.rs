//! The one task that owns every chat on this machine: it takes people's commands, starts and
//! orders agents, commits what happens to each chat's log and then publishes it.

pub(crate) mod agent;
pub(super) mod busy;
#[cfg(test)]
mod by_hand;
mod commands;
mod cut;
mod handle;
mod hold;
mod launch;
mod pending;
mod queries;
mod rest;
mod said;
mod turns;
mod unwritten;

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use tokio::sync::{mpsc, watch};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AgentSession, ChatEntry, ChatEvent, ChatUpdate, Message, Outcome, Receipt, RequestId, RunId,
    RunState, RunStatus, code,
};

use self::agent::{Agent, FromRun, Order};
pub(crate) use self::busy::{Install, LeaveId, LeaveStep};
use self::handle::Ask;
pub(crate) use self::handle::{Chats, WeakChats};
use self::launch::Launched;
pub(crate) use self::launch::{Host, RunAs};
use self::pending::{PendingAnswer, PendingEnd, PendingInterrupt};
use super::feed::{Client, Subscriber};
use super::log::ChatLog;
use crate::runs::{ChatKey, Runs};
use crate::workspaces::Workspaces;

/// How many clients' asks may wait for the engine.
const ASKS: usize = 64;
/// How many events from the chats' runs may wait for the engine.
const SAYS: usize = 256;
const IDLE_CHECK: std::time::Duration = std::time::Duration::from_mins(1);

fn unsupported() -> Fault {
    Fault::new(
        code!("message.unsupported"),
        "This build doesn't know that message",
    )
}

/// A chat the engine has open: one with a running agent, a waiting prompt or a subscriber.
struct Chat {
    log: ChatLog,
    subscribers: Vec<Subscriber>,
    /// The way to the chat's agent: none without one, closed once it stopped reading, until
    /// its exit is reported.
    orders: Option<mpsc::UnboundedSender<Order>>,
    /// The run whose agent is starting, off the engine's loop.
    launching: Option<RunId>,
    /// A run let go after a failed write whose agent is still starting.
    let_go: Option<RunId>,
    pending_answers: Vec<PendingAnswer>,
    pending_end: Option<PendingEnd>,
    /// Interrupts committed or tried and not yet sent, each bound to the turn it stops.
    pending_interrupts: Vec<PendingInterrupt>,
    /// The agent's session when the runs journal couldn't save it.
    pending_session: Option<AgentSession>,
    /// The local client carrying the chat on by hand: no turn starts while it holds it.
    held: Option<Client>,
}

impl Chat {
    // Lets go of clients that hung up; whether one of them held the chat.
    fn forget_gone(&mut self) -> bool {
        self.subscribers
            .retain(|subscriber| !subscriber.client().is_gone());
        self.held.take_if(|holder| holder.is_gone()).is_some()
    }

    // Whether an agent is starting for the chat, even one let go after a failed write.
    const fn is_starting(&self) -> bool {
        self.launching.is_some() || self.let_go.is_some()
    }

    // Whether nothing needs the chat open.
    fn is_unused(&self) -> bool {
        self.orders.is_none()
            && !self.is_starting()
            && self.pending_interrupts.is_empty()
            && self.subscribers.is_empty()
            && self.pending_answers.is_empty()
            && self.pending_end.is_none()
            && self.pending_session.is_none()
            && self.held.is_none()
    }
}

pub(crate) struct Engine<A> {
    runs: Runs,
    workspaces: Workspaces,
    open_chats: HashMap<ChatKey, Chat>,
    host: Host<A>,
    asked: mpsc::Receiver<Ask>,
    incoming: mpsc::Receiver<FromRun>,
    says: mpsc::Sender<FromRun>,
    /// The disk's state as last logged; turns wait on `disk`, never on this.
    disk_full: bool,
    disk: watch::Receiver<bool>,
    /// While an install has leave, prompts are queued and no new turn starts.
    install: Install,
    starts: mpsc::UnboundedSender<Launched<A>>,
    started: mpsc::UnboundedReceiver<Launched<A>>,
    log: Logger,
}

impl<A: Agent> Engine<A> {
    pub(crate) fn new(
        runs: Runs,
        workspaces: Workspaces,
        host: Host<A>,
        log: Logger,
    ) -> (Chats, Self) {
        let (asks, asked) = mpsc::channel(ASKS);
        let (says, incoming) = mpsc::channel(SAYS);
        let (starts, started) = mpsc::unbounded_channel();
        let (disk_full, disk) = watch::channel(false);
        let engine = Self {
            runs,
            workspaces,
            open_chats: HashMap::new(),
            host,
            asked,
            incoming,
            says,
            disk_full: false,
            install: Install::default(),
            disk,
            starts,
            started,
            log,
        };
        (Chats::new(asks, disk_full), engine)
    }

    /// Serves until every [`Chats`] is dropped; the agents stop with it.
    pub(crate) async fn serve(mut self) {
        self.reconcile().await;
        let mut ticks = tokio::time::interval(IDLE_CHECK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let key = tokio::select! {
                _ = ticks.tick() => {
                    self.sleep_idle().await;
                    continue;
                }
                ask = self.asked.recv() => match ask {
                    Some(ask) => match self.answer(ask).await {
                        Some(key) => key,
                        None => continue,
                    },
                    None => return self.log.info("engine stopped", &[]),
                },
                Some(said) = self.incoming.recv() => self.take_said(said).await,
                Ok(()) = self.disk.changed() => {
                    let full = *self.disk.borrow_and_update();
                    self.set_disk_full(full).await;
                    continue;
                }
                Some(launched) = self.started.recv() => self.take_launched(launched).await,
                () = self.install.runs_out(), if self.install.pauses() => {
                    self.install.pause = None;
                    self.turns_resumed().await;
                    continue;
                }
            };
            self.close_if_unused(&key);
        }
    }

    async fn set_state(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        state: RunState,
    ) -> Result<(), Fault> {
        self.runs.update(key, |record| record.state = state).await?;
        self.tell_state(key, run, state).await
    }

    // Once nothing of `run`, the chat's own run, waits for a person, its turn is running again.
    async fn running_again(&mut self, key: &ChatKey, run: &RunId) -> Result<(), Fault> {
        let log = &self.open(key).await?.log;
        let asks = log.runs().asks(run);
        let running = self
            .runs
            .of(key)
            .is_some_and(|record| record.state == RunState::Running);
        let current = self.run_of(key).as_ref() == Some(run);
        if asks || running || !current || self.turn(key).is_none() {
            return Ok(());
        }
        self.set_state(key, run, RunState::Running).await
    }

    // Commits the run's state for the chat's subscribers; the journal already holds it.
    async fn tell_state(
        &mut self,
        key: &ChatKey,
        run: &RunId,
        state: RunState,
    ) -> Result<(), Fault> {
        let status = RunStatus {
            run: run.clone(),
            state,
            agent: None,
        };
        self.commit(key, ChatEvent::RunStatus { status }).await
    }

    async fn commit_receipt(
        &mut self,
        key: &ChatKey,
        request: &RequestId,
        outcome: Outcome,
    ) -> Result<(), Fault> {
        // A command's end is told once, whichever path gets there first.
        if self.open(key).await?.log.has_ended(request) {
            return Ok(());
        }
        let receipt = Receipt {
            request: request.clone(),
            outcome,
        };
        self.commit(key, ChatEvent::Receipt { receipt }).await
    }

    // Commits `event` to the chat's log, then tells its subscribers.
    async fn commit(&mut self, key: &ChatKey, event: ChatEvent) -> Result<(), Fault> {
        let chat = self.open(key).await?;
        match chat.log.append(event).await {
            Ok(entry) => {
                self.publish(key, &update(key, entry));
                Ok(())
            }
            Err(fault) => {
                if fault.code() == &code!("chat.log_not_written") {
                    self.mend(key).await;
                }
                Err(fault)
            }
        }
    }

    // Opens a log again after a failed write; its subscribers start over from a snapshot.
    async fn mend(&mut self, key: &ChatKey) {
        let root = self.workspaces.root(&key.workspace);
        let Some(chat) = self.open_chats.get_mut(key) else {
            return;
        };
        for subscriber in chat.subscribers.drain(..) {
            subscriber.end();
        }
        if let Ok(log) = ChatLog::open(&root, &key.chat).await {
            chat.log = log;
        }
    }

    fn publish(&mut self, key: &ChatKey, message: &Message) {
        if let Some(chat) = self.open_chats.get_mut(key) {
            chat.subscribers = std::mem::take(&mut chat.subscribers)
                .into_iter()
                .filter_map(|subscriber| subscriber.tell(message.clone()))
                .collect();
        }
    }

    // Whether the order reached the run; a run that is gone says so itself, in its last report.
    fn order(&self, key: &ChatKey, order: Order) -> bool {
        self.open_chats
            .get(key)
            .and_then(|chat| chat.orders.as_ref())
            .is_some_and(|orders| orders.send(order).is_ok())
    }

    // Whether the chat's run hasn't yet reported its exit.
    fn is_live(&self, key: &ChatKey) -> bool {
        self.open_chats
            .get(key)
            .is_some_and(|chat| chat.orders.is_some())
    }

    // Whether the chat's run still reads orders: one that stopped reading reports its exit next.
    fn takes_orders(&self, key: &ChatKey) -> bool {
        self.open_chats
            .get(key)
            .and_then(|chat| chat.orders.as_ref())
            .is_some_and(|orders| !orders.is_closed())
    }

    fn now(&self) -> u64 {
        (self.host.now)()
    }

    fn turn(&self, key: &ChatKey) -> Option<RequestId> {
        self.runs.of(key)?.turn.clone()
    }

    fn run_of(&self, key: &ChatKey) -> Option<RunId> {
        self.runs.of(key)?.run.clone()
    }

    async fn open(&mut self, key: &ChatKey) -> Result<&mut Chat, Fault> {
        let root = self.workspaces.root(&key.workspace);
        match self.open_chats.entry(key.clone()) {
            Entry::Occupied(open) => Ok(open.into_mut()),
            Entry::Vacant(closed) => Ok(closed.insert(Chat {
                log: ChatLog::open(&root, &key.chat).await?,
                subscribers: Vec::new(),
                orders: None,
                launching: None,
                let_go: None,
                pending_answers: Vec::new(),
                pending_end: None,
                pending_interrupts: Vec::new(),
                pending_session: None,
                held: None,
            })),
        }
    }

    // Closes a chat nobody is using, so its log doesn't stay open for good. A held one is left
    // to `close_unused`, which takes its prompts on once its holder is gone.
    fn close_if_unused(&mut self, key: &ChatKey) {
        let Some(chat) = self
            .open_chats
            .get_mut(key)
            .filter(|chat| chat.held.is_none())
        else {
            return;
        };
        chat.forget_gone();
        if chat.is_unused() {
            self.open_chats.remove(key);
        }
    }
}

// What names a chat's run on a log line.
fn about(key: &ChatKey, run: &RunId) -> [(&'static str, Field); 3] {
    let [workspace, chat] = chat_about(key);
    [workspace, chat, ("run_id", run.as_str().into())]
}

// What names a chat on a log line.
fn chat_about(key: &ChatKey) -> [(&'static str, Field); 2] {
    [
        ("workspace_id", key.workspace.as_str().into()),
        ("chat_id", key.chat.as_str().into()),
    ]
}

// What names a chat's run on a log line, and the fault it met.
fn failed(key: &ChatKey, run: &RunId, fault: &Fault) -> [(&'static str, Field); 4] {
    let [workspace, chat, run] = about(key, run);
    [workspace, chat, run, ("err", Field::error(fault))]
}

fn update(key: &ChatKey, entry: ChatEntry) -> Message {
    Message::Chat(ChatUpdate {
        workspace: key.workspace.clone(),
        chat: key.chat.clone(),
        entry,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use weacon_model::ChatRuns;
    use weacon_protocol::{AgentKind, ChatAction, ChatAnswerKind, ChatQueryKind, RunEvent};

    use super::agent::Said;
    use super::by_hand::on_turn;
    use super::*;
    use crate::chats::test_support::{
        Bench, Started, Told, allow, asks, bench, chat, client, command, key, parts, prompt,
        prompt_naming, pushed, query, run, session, skip, soon, text, workspace,
    };

    fn done(outcome: Outcome) -> RunEvent {
        RunEvent::Finished { outcome }
    }

    // The chat's snapshot: its runs, its waiting prompts and its latest events.
    async fn snapshot(bench: &Bench) -> (Vec<RunStatus>, Vec<u64>, Vec<ChatEntry>) {
        let (anyone, _pushes) = client(1);
        let asked = query("c-1", ChatQueryKind::Snapshot);
        let Ok(Some(ChatAnswerKind::Snapshot {
            runs,
            waiting,
            events,
            ..
        })) = bench.chats.query(asked, &anyone).await
        else {
            panic!("no snapshot");
        };
        (runs, waiting, events)
    }

    // A client subscribed to chat c-1 from its start.
    async fn watch(bench: &Bench) -> mpsc::Receiver<Message> {
        let (client, pushes) = client(64);
        let subscribe = query("c-1", ChatQueryKind::Subscribe { after: 0 });
        assert_eq!(bench.chats.query(subscribe, &client).await.unwrap(), None);
        pushes
    }

    // Sends the chat's first prompt and returns the agent started for it.
    async fn first_turn(bench: &mut Bench, pushes: &mut mpsc::Receiver<Message>) -> Started {
        bench.command(prompt("r-1", "Hi")).await.unwrap();
        assert_eq!(
            pushed(pushes, 4).await,
            [
                "1 prompt r-1 Hi",
                "2 receipt r-1 accepted",
                "3 run-1 is Starting",
                "4 run-1 is Running",
            ]
        );
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
        started
    }

    fn code_of(refused: Result<(), Fault>) -> String {
        refused.unwrap_err().code().as_str().to_owned()
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_run_is_busy_on_a_turn_and_not_before_or_between_turns() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        assert!(!bench.chats.downgrade().busy().await.unwrap(), "no run");

        let agent = first_turn(&mut bench, &mut pushes).await;
        assert!(bench.chats.downgrade().busy().await.unwrap(), "on a turn");
        agent.say.send(done(Outcome::Completed)).unwrap();
        pushed(&mut pushes, 3).await;
        assert!(
            !bench.chats.downgrade().busy().await.unwrap(),
            "idle between turns"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_starts_a_run_and_its_turn_is_committed_then_published_in_order() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;

        let mut agent = first_turn(&mut bench, &mut pushes).await;
        let run_as = &agent.run_as;
        assert_eq!(
            (&run_as.key, run_as.agent, &run_as.run),
            (&key(), AgentKind::Claude, &run("run-1"))
        );
        assert!(run_as.session.is_none() && run_as.repo.is_none() && run_as.task.is_none());
        let delta = RunEvent::TextDelta {
            text: "Hel".to_owned(),
        };
        let says = [
            RunEvent::Started { session: session() },
            delta,
            text("Hello."),
            done(Outcome::Completed),
        ];
        for event in says {
            agent.say.send(event).unwrap();
        }

        assert_eq!(
            pushed(&mut pushes, 6).await,
            [
                "5 run-1 started",
                "live run-1 delta Hel",
                "6 run-1 text Hello.",
                "7 run-1 finished completed",
                "8 receipt r-1 completed",
                "9 run-1 is Idle",
            ]
        );

        bench.command(prompt("r-2", "More")).await.unwrap();
        assert_eq!(
            pushed(&mut pushes, 3).await,
            [
                "10 prompt r-2 More",
                "11 receipt r-2 accepted",
                "12 run-1 is Running"
            ]
        );
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("More".to_owned()))
        );
        bench.started.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_sent_during_a_turn_waits_for_it_to_end() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let mut agent = first_turn(&mut bench, &mut pushes).await;

        bench.command(prompt("r-2", "Then this")).await.unwrap();
        assert_eq!(
            pushed(&mut pushes, 2).await,
            ["5 prompt r-2 Then this", "6 receipt r-2 accepted"]
        );
        agent.told.try_recv().unwrap_err();
        agent.say.send(done(Outcome::Completed)).unwrap();

        assert_eq!(
            pushed(&mut pushes, 4).await,
            [
                "7 run-1 finished completed",
                "8 receipt r-1 completed",
                "9 run-1 is Idle",
                "10 run-1 is Running",
            ]
        );
        assert_eq!(
            soon(agent.told.recv()).await,
            Some(Told::Send("Then this".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_repeated_request_does_nothing_a_conflicting_one_is_refused_and_an_empty_one_fails() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let mut agent = first_turn(&mut bench, &mut pushes).await;

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        let conflict = bench.command(prompt("r-1", "Bye")).await;
        for _ in 0..2 {
            bench.command(prompt("r-2", " \n")).await.unwrap();
        }
        let other = bench.command(prompt("r-2", "Deploy")).await;
        bench.command(prompt("r-3", "Next")).await.unwrap();

        assert_eq!(code_of(conflict), "request.conflict");
        assert_eq!(code_of(other), "request.conflict");
        assert_eq!(
            pushed(&mut pushes, 2).await,
            [
                "5 receipt r-2 failed chat.empty_prompt",
                "6 prompt r-3 Next"
            ]
        );
        agent.told.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_answer_only_reaches_the_run_that_asked() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let mut agent = first_turn(&mut bench, &mut pushes).await;
        agent
            .say
            .send(RunEvent::Started { session: session() })
            .unwrap();
        agent.say.send(asks("run-1", 1)).unwrap();
        assert_eq!(
            pushed(&mut pushes, 3).await,
            ["5 run-1 started", "6 run-1 asks 1", "7 run-1 is Waiting"]
        );

        for wrong in [
            allow("r-2", "c-1", "run-9", 1),
            allow("r-2", "c-1", "run-1", 2),
            allow("r-2", "c-2", "run-1", 1),
        ] {
            assert_eq!(code_of(bench.command(wrong).await), "agent.no_such_prompt");
        }
        agent.told.try_recv().unwrap_err();

        bench
            .command(allow("r-2", "c-1", "run-1", 1))
            .await
            .unwrap();
        assert_eq!(
            pushed(&mut pushes, 3).await,
            [
                "8 receipt r-2 completed",
                "9 run-1 answered 1",
                "10 run-1 is Running"
            ]
        );
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.allow)
        );
        let again = bench.command(allow("r-3", "c-1", "run-1", 1)).await;
        assert_eq!(code_of(again), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_cut_off_with_its_run_is_never_answered_in_the_chats_next_run() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let agent = first_turn(&mut bench, &mut pushes).await;
        let started = RunEvent::Started { session: session() };
        agent.say.send(started).unwrap();
        agent.say.send(asks("run-1", 2)).unwrap();
        drop(agent.say);
        assert_eq!(
            pushed(&mut pushes, 6).await,
            [
                "5 run-1 started",
                "6 run-1 asks 2",
                "7 run-1 is Waiting",
                "8 run-1 finished unknown",
                "9 receipt r-1 unknown",
                "10 run-1 is Sleeping",
            ]
        );
        bench.command(prompt("r-4", "Again")).await.unwrap();
        let mut next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            (next.run_as.run.as_str(), next.run_as.session.clone()),
            ("run-2", Some(session()))
        );
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("Again".to_owned()))
        );
        let late = bench.command(allow("r-5", "c-1", "run-1", 2)).await;
        assert_eq!(code_of(late), "agent.no_such_prompt");
        next.told.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawn_permission_stops_waiting_and_an_interrupt_ends_the_turn() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let mut agent = first_turn(&mut bench, &mut pushes).await;
        agent.say.send(asks("run-1", 1)).unwrap();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 1,
        };
        agent.say.send(withdrawn).unwrap();
        assert_eq!(
            pushed(&mut pushes, 4).await,
            [
                "5 run-1 asks 1",
                "6 run-1 is Waiting",
                "7 run-1 withdraws 1",
                "8 run-1 is Running",
            ]
        );
        let late = bench.command(allow("r-2", "c-1", "run-1", 1)).await;
        assert_eq!(code_of(late), "agent.no_such_prompt");

        let interrupt = command("r-3", "c-1", ChatAction::Interrupt);
        bench.command(interrupt).await.unwrap();
        assert_eq!(soon(agent.told.recv()).await, Some(Told::Interrupt));
        agent.say.send(done(Outcome::Interrupted)).unwrap();
        assert_eq!(
            pushed(&mut pushes, 4).await,
            [
                "9 receipt r-3 completed",
                "10 run-1 finished interrupted",
                "11 receipt r-1 interrupted",
                "12 run-1 is Idle",
            ]
        );

        let idle = command("r-4", "c-1", ChatAction::Interrupt);
        bench.command(idle).await.unwrap();
        assert_eq!(pushed(&mut pushes, 1).await, ["13 receipt r-4 completed"]);
        agent.told.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_chats_first_prompt_fixes_its_agent_and_a_later_one_cant_change_it() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;

        let first = prompt_naming("r-1", "Hi", AgentKind::Codex);
        bench.command(first).await.unwrap();

        let started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.run_as.agent, AgentKind::Codex);
        skip(&mut pushes, 4).await;
        let other = prompt_naming("r-2", "More", AgentKind::Claude);
        bench.command(other).await.unwrap();
        let same = prompt_naming("r-3", "More", AgentKind::Codex);
        bench.command(same).await.unwrap();
        assert_eq!(
            pushed(&mut pushes, 2).await,
            [
                "5 receipt r-2 failed chat.agent_conflict",
                "6 prompt r-3 More"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_naming_an_agent_this_build_doesnt_know_fails() {
        let bench = bench();
        let mut pushes = watch(&bench).await;

        let newer = prompt_naming("r-1", "Hi", AgentKind::Newer);
        bench.command(newer).await.unwrap();

        let failed = pushed(&mut pushes, 1).await;
        assert_eq!(failed, ["1 receipt r-1 failed message.unsupported"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_that_cant_start_fails_the_prompt_and_the_next_one_tries_again() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        bench.broken.store(true, Ordering::SeqCst);

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        assert_eq!(
            pushed(&mut pushes, 4).await,
            [
                "1 prompt r-1 Hi",
                "2 receipt r-1 accepted",
                "3 run-1 is Ended",
                "4 receipt r-1 failed agent.not_started",
            ]
        );
        bench.broken.store(false, Ordering::SeqCst);
        bench.command(prompt("r-2", "Again")).await.unwrap();

        // The failed prompt left the queue: the new run gets only the next one.
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.run_as.run, run("run-2"));
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Again".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_that_exits_leaves_its_turn_unknown_and_queued_prompts_get_a_new_run() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let agent = first_turn(&mut bench, &mut pushes).await;
        bench.command(prompt("r-2", "Second")).await.unwrap();
        bench.command(prompt("r-3", "Third")).await.unwrap();
        skip(&mut pushes, 4).await;

        drop(agent);
        assert_eq!(
            pushed(&mut pushes, 5).await,
            [
                "9 run-1 finished unknown",
                "10 receipt r-1 unknown",
                "11 run-1 is Sleeping",
                "12 run-2 is Starting",
                "13 run-2 is Running",
            ]
        );
        let mut second = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(second.told.recv()).await,
            Some(Told::Send("Second".to_owned()))
        );

        // The new run exits too and nothing can start: what is queued fails rather than waits.
        bench.broken.store(true, Ordering::SeqCst);
        drop(second.say);
        assert_eq!(
            pushed(&mut pushes, 5).await,
            [
                "14 run-2 finished unknown",
                "15 receipt r-2 unknown",
                "16 run-2 is Sleeping",
                "17 run-3 is Ended",
                "18 receipt r-3 failed agent.not_started",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_missed_events_is_told_and_subscribes_again() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let agent = first_turn(&mut bench, &mut pushes).await;

        // Room for one event beside the place kept for the notice.
        let (slow, mut slowly) = client(2);
        let again = query("c-1", ChatQueryKind::Subscribe { after: 4 });
        assert_eq!(bench.chats.query(again, &slow).await.unwrap(), None);
        for said in ["One", "Two", "Three"] {
            agent.say.send(text(said)).unwrap();
        }
        skip(&mut pushes, 3).await;
        assert_eq!(
            pushed(&mut slowly, 2).await,
            ["5 run-1 text One", "events lost"]
        );
        slowly.try_recv().unwrap_err();

        let ahead = query("c-1", ChatQueryKind::Subscribe { after: 8 });
        let lost = bench.chats.query(ahead, &slow).await.unwrap();
        assert_eq!(lost, Some(ChatAnswerKind::EventsLost));
        let caught_up = query("c-1", ChatQueryKind::Subscribe { after: 6 });
        assert_eq!(bench.chats.query(caught_up, &slow).await.unwrap(), None);
        agent.say.send(text("Four")).unwrap();
        assert_eq!(
            pushed(&mut slowly, 2).await,
            ["7 run-1 text Three", "8 run-1 text Four"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_whose_agent_is_slow_to_start_holds_up_no_other() {
        let mut bench = bench();
        let release = bench.hold_next_start();

        let sent = soon(async { Some(bench.command(prompt("r-1", "Hi")).await) }).await;
        assert!(matches!(sent, Some(Ok(()))));
        assert!(bench.chats.downgrade().busy().await.unwrap(), "starting");
        let (anyone, _pushes) = client(1);
        let elsewhere = query("c-2", ChatQueryKind::Snapshot);
        let answer = soon(async { Some(bench.chats.query(elsewhere, &anyone).await) }).await;
        assert!(matches!(
            answer,
            Some(Ok(Some(ChatAnswerKind::Snapshot { seq: 0, .. })))
        ));

        drop(release);
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Hi".to_owned()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_disk_queues_prompts_and_starts_their_turns_once_it_has_room() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        bench.chats.disk_full(true);

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        let accepted = pushed(&mut pushes, 2).await;
        assert_eq!(accepted, ["1 prompt r-1 Hi", "2 receipt r-1 accepted"]);
        assert!(soon(bench.started.recv()).await.is_none());

        bench.chats.disk_full(false);
        let mut started = soon(bench.started.recv()).await.unwrap();
        let told = soon(started.told.recv()).await;
        assert_eq!(told, Some(Told::Send("Hi".to_owned())));
        let running = pushed(&mut pushes, 2).await;
        assert_eq!(running, ["3 run-1 is Starting", "4 run-1 is Running"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_sent_while_the_agent_starts_stops_the_turn_it_was_starting_for() {
        let mut bench = bench();
        let release = bench.hold_next_start();
        bench.command(prompt("r-1", "Hi")).await.unwrap();

        let interrupt = command("r-2", "c-1", ChatAction::Interrupt);
        bench.command(interrupt).await.unwrap();
        drop(release);

        let mut started = soon(bench.started.recv()).await.unwrap();
        let told = [
            soon(started.told.recv()).await,
            soon(started.told.recv()).await,
        ];
        assert_eq!(
            told,
            [Some(Told::Send("Hi".to_owned())), Some(Told::Interrupt)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_disk_is_heard_even_when_the_engine_is_busy() {
        let (mut bench, engine) = parts();
        let (anyone, _pushes) = client(1);
        // Enough queries to fill the engine's queue before it serves.
        let ask = |chats: Chats, anyone: Client| async move {
            let snapshot = query("c-2", ChatQueryKind::Snapshot);
            chats.query(snapshot, &anyone).await
        };
        let asked: Vec<_> = (0..ASKS)
            .map(|_| tokio::spawn(ask(bench.chats.clone(), anyone.clone())))
            .collect();
        tokio::task::yield_now().await;

        bench.chats.disk_full(true);
        tokio::spawn(engine.serve());
        for asked in asked {
            asked.await.unwrap().unwrap();
        }

        bench.command(prompt("r-1", "Hi")).await.unwrap();
        assert!(soon(bench.started.recv()).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn reading_a_chat_this_machine_never_had_writes_nothing() {
        let (_bench, mut engine) = parts();
        let root = engine.workspaces.root(&workspace());
        let (client, mut pushes) = client(8);

        let snapshot = engine.query(&key(), ChatQueryKind::Snapshot, &client).await;
        let subscribe = ChatQueryKind::Subscribe { after: 0 };
        engine.query(&key(), subscribe, &client).await.unwrap();

        assert!(
            matches!(snapshot, Ok(Some(ChatAnswerKind::Snapshot { seq: 0, ref events, .. })) if events.is_empty())
        );
        assert!(!root.exists());
        engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
        assert_eq!(pushed(&mut pushes, 1).await, ["1 prompt r-1 Hi"]);
        assert!(root.join("chats").is_dir());
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_tells_the_runs_the_waiting_prompts_and_the_latest_events() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let (anyone, _pushes) = client(2);
        let empty = query("c-2", ChatQueryKind::Snapshot);
        let nothing = ChatAnswerKind::Snapshot {
            seq: 0,
            runs: Vec::new(),
            waiting: Vec::new(),
            events: Vec::new(),
        };
        assert_eq!(
            bench.chats.query(empty, &anyone).await.unwrap(),
            Some(nothing)
        );
        let agent = first_turn(&mut bench, &mut pushes).await;
        agent.say.send(asks("run-1", 1)).unwrap();
        skip(&mut pushes, 2).await;

        let snapshot = query("c-1", ChatQueryKind::Snapshot);
        let Ok(Some(ChatAnswerKind::Snapshot {
            seq,
            runs,
            waiting,
            events,
        })) = bench.chats.query(snapshot, &anyone).await
        else {
            panic!("no snapshot");
        };
        let waits = RunStatus {
            run: run("run-1"),
            state: RunState::Waiting,
            agent: None,
        };
        assert_eq!((seq, waiting, runs), (6, vec![5], vec![waits]));
        let seqs: Vec<u64> = events.iter().map(|entry| entry.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5, 6]);

        let older = ChatQueryKind::Window {
            before: 3,
            limit: 9,
        };
        let window = bench.chats.query(query("c-1", older), &anyone).await;
        assert!(matches!(window, Ok(Some(ChatAnswerKind::Window { events })) if events.len() == 2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_keeps_waiting_until_its_last_prompt_is_answered() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let mut agent = first_turn(&mut bench, &mut pushes).await;
        agent.say.send(asks("run-1", 1)).unwrap();
        agent.say.send(asks("run-1", 2)).unwrap();
        skip(&mut pushes, 4).await;

        let first = allow("r-2", "c-1", "run-1", 1);
        bench.command(first).await.unwrap();
        // Of the two prompts, only the second still waits, as the fold of the log says.
        let (runs, waiting, events) = snapshot(&bench).await;
        let folded = ChatRuns::fold(&events);
        assert_eq!((waiting.as_slice(), runs.len()), ([7].as_slice(), 1));
        let folded_runs: Vec<_> = folded.run().cloned().into_iter().collect();
        assert_eq!((folded.waiting_seqs(), folded_runs), (waiting, runs));
        let second = allow("r-3", "c-1", "run-1", 2);
        bench.command(second).await.unwrap();

        assert_eq!(
            pushed(&mut pushes, 5).await,
            [
                "9 receipt r-2 completed",
                "10 run-1 answered 1",
                "11 receipt r-3 completed",
                "12 run-1 answered 2",
                "13 run-1 is Running",
            ]
        );
        assert!(snapshot(&bench).await.1.is_empty());
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.prompt == 1)
        );
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.prompt == 2)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawn_permission_leaves_the_run_waiting_while_another_waits() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let agent = first_turn(&mut bench, &mut pushes).await;
        agent.say.send(asks("run-1", 1)).unwrap();
        agent.say.send(asks("run-1", 2)).unwrap();
        let withdrawn = RunEvent::PermissionWithdrawn {
            run: run("run-1"),
            prompt: 2,
        };
        agent.say.send(withdrawn).unwrap();
        agent.say.send(text("Still waiting")).unwrap();

        assert_eq!(
            pushed(&mut pushes, 6).await,
            [
                "5 run-1 asks 1",
                "6 run-1 is Waiting",
                "7 run-1 asks 2",
                "8 run-1 is Waiting",
                "9 run-1 withdraws 2",
                "10 run-1 text Still waiting",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_has_one_subscription_to_a_chat_and_a_full_queue_gets_none() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let (twice, mut once) = client(8);
        for _ in 0..2 {
            let subscribe = query("c-1", ChatQueryKind::Subscribe { after: 0 });
            assert_eq!(bench.chats.query(subscribe, &twice).await.unwrap(), None);
        }
        let (full, mut filled) = client(1);
        let subscribe = || query("c-1", ChatQueryKind::Subscribe { after: 0 });
        assert_eq!(bench.chats.query(subscribe(), &full).await.unwrap(), None);

        let _agent = first_turn(&mut bench, &mut pushes).await;

        skip(&mut once, 4).await;
        once.try_recv().unwrap_err();
        let lost = bench.chats.query(subscribe(), &full).await.unwrap();
        assert_eq!(lost, Some(ChatAnswerKind::EventsLost));
        assert_eq!(pushed(&mut filled, 1).await, ["events lost"]);
    }

    #[tokio::test(start_paused = true)]
    async fn what_this_build_doesnt_know_is_refused_and_a_stopped_engine_says_so() {
        let bench = bench();
        let (anyone, _pushes) = client(2);
        let mut pushes = watch(&bench).await;

        let action = bench
            .chats
            .command(command("r-1", "c-1", ChatAction::Newer), &anyone);
        action.await.unwrap();
        let failed = pushed(&mut pushes, 1).await;
        assert_eq!(failed, ["1 receipt r-1 failed message.unsupported"]);
        let asked = bench
            .chats
            .query(query("c-1", ChatQueryKind::Newer), &anyone);
        assert_eq!(
            asked.await.unwrap_err().code().as_str(),
            "message.unsupported"
        );

        let (stopped, engine) = parts();
        drop(engine);
        let command = stopped.command(prompt("r-1", "Hi")).await;
        assert_eq!(code_of(command), "chat.engine_stopped");
        let snapshot = query("c-1", ChatQueryKind::Snapshot);
        let asked = stopped.chats.query(snapshot, &anyone).await;
        assert_eq!(asked.unwrap_err().code().as_str(), "chat.engine_stopped");
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_stays_open_only_while_an_agent_a_prompt_or_a_client_needs_it() {
        let mut on = on_turn().await;
        let other = ChatKey {
            chat: chat("c-2"),
            ..key()
        };
        let (watcher, watching) = client(4);
        let subscribe = || ChatQueryKind::Subscribe { after: 0 };
        on.engine
            .query(&other, subscribe(), &watcher)
            .await
            .unwrap();

        on.engine.close_if_unused(&key());
        on.engine.close_if_unused(&other);
        assert!(
            on.engine.open_chats.contains_key(&key()) && on.engine.open_chats.contains_key(&other)
        );

        // The agent exits, and both clients hang up.
        drop(on.agent);
        on.engine
            .hear(&key(), &run("run-1"), Said::Exited(None))
            .await
            .unwrap();
        on.engine.close_if_unused(&key());
        assert!(
            on.engine.open_chats.contains_key(&key()),
            "a client still watches it"
        );
        drop((on.pushes, watching));
        on.engine.close_if_unused(&key());
        assert!(!on.engine.open_chats.contains_key(&key()));

        // A hung-up client's chat is let go when anyone subscribes anywhere.
        assert!(on.engine.open_chats.contains_key(&other));
        let (third, _watching) = client(4);
        let elsewhere = ChatKey {
            chat: chat("c-3"),
            ..key()
        };
        on.engine
            .query(&elsewhere, subscribe(), &third)
            .await
            .unwrap();
        assert!(!on.engine.open_chats.contains_key(&other));
        assert!(on.engine.open_chats.contains_key(&elsewhere));
    }
}
