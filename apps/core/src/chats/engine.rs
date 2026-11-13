//! The one task that owns every chat on this machine: it takes people's commands, starts and
//! orders agents, commits what happens to each chat's log and then publishes it.

mod commands;
mod queries;
mod turns;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AgentKind, AgentSession, ChatAnswerKind, ChatCommand, ChatEntry, ChatEvent, ChatId, ChatQuery,
    ChatUpdate, Message, Outcome, Receipt, RequestId, RunId, RunState, RunStatus, WorkspaceId,
    code,
};

use super::feed::{Client, Subscriber};
use super::log::Log;
use crate::runs::Runs;
use crate::runs::task::{Agent, Order, Report};
use crate::workspaces::Workspaces;

const ASKS: usize = 64;
const REPORTS: usize = 256;

type Key = (WorkspaceId, ChatId);

/// Which agent to start, and as what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Start {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    pub(crate) agent: AgentKind,
    pub(crate) run: RunId,
    pub(crate) session: Option<AgentSession>,
}

type Starter<A> = Arc<dyn Fn(&Start) -> Result<A, Fault> + Send + Sync>;

/// What the engine takes from the machine; the daemon wires the real ones.
pub(crate) struct Host<A> {
    /// Starts an agent; may block, so it is called off the runtime.
    pub(crate) start: Starter<A>,
    /// Unix seconds.
    pub(crate) now: fn() -> u64,
}

/// The clients' way to the engine.
#[derive(Clone, Debug)]
pub(crate) struct Chats {
    asks: mpsc::Sender<Ask>,
}

enum Ask {
    Command(ChatCommand, oneshot::Sender<Result<(), Fault>>),
    Query(
        ChatQuery,
        Client,
        oneshot::Sender<Result<Option<ChatAnswerKind>, Fault>>,
    ),
}

fn stopped() -> Fault {
    Fault::new(code!("chat.engine_stopped"), "The daemon is stopping")
}

fn unsupported() -> Fault {
    Fault::new(
        code!("message.unsupported"),
        "This build doesn't know that message",
    )
}

impl Chats {
    /// Acts on `command`. Its receipts are events in the chat's log; an error means it has no
    /// receipt, and sending it again is safe.
    pub(crate) async fn command(&self, command: ChatCommand) -> Result<(), Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Command(command, answer);
        self.asks.send(ask).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())?
    }

    /// The answer to `query`, if it has one; a subscription's events go to `client`.
    pub(crate) async fn query(
        &self,
        query: ChatQuery,
        client: &Client,
    ) -> Result<Option<ChatAnswerKind>, Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask::Query(query, client.clone(), answer);
        self.asks.send(ask).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| stopped())?
    }
}

/// A chat the engine has open: one with a running agent, a waiting prompt or a subscriber.
struct Chat {
    log: Log,
    subscribers: Vec<Subscriber>,
    /// The way to the chat's running agent.
    orders: Option<mpsc::UnboundedSender<Order>>,
    waiting: Vec<Waiting>,
    /// The outcome the run reported for its turn when the turn's receipt couldn't be written.
    unended: Option<Outcome>,
    /// An interrupt's request, and the turn it stops, until the interrupt is sent.
    interrupting: Option<(RequestId, RequestId)>,
}

impl Chat {
    // Whether no agent, waiting prompt or connected subscriber needs the chat open.
    fn is_idle(&mut self) -> bool {
        self.subscribers.retain(|subscriber| !subscriber.is_gone());
        self.orders.is_none() && self.subscribers.is_empty() && self.waiting.is_empty()
    }
}

/// A permission prompt still waiting for a person, and its place in the log.
struct Waiting {
    prompt: u32,
    seq: u64,
    /// The request, and whether it allowed, that answered it once its receipt is written.
    answered: Option<(RequestId, bool)>,
}

pub(crate) struct Engine<A> {
    runs: Runs,
    workspaces: Workspaces,
    chats: HashMap<Key, Chat>,
    host: Host<A>,
    asks: mpsc::Receiver<Ask>,
    reports: mpsc::Receiver<Report>,
    reporter: mpsc::Sender<Report>,
    log: Logger,
}

impl<A: Agent> Engine<A> {
    pub(crate) fn new(
        runs: Runs,
        workspaces: Workspaces,
        host: Host<A>,
        log: Logger,
    ) -> (Chats, Self) {
        let (asks_in, asks) = mpsc::channel(ASKS);
        let (reporter, reports) = mpsc::channel(REPORTS);
        let engine = Self {
            runs,
            workspaces,
            chats: HashMap::new(),
            host,
            asks,
            reports,
            reporter,
            log,
        };
        (Chats { asks: asks_in }, engine)
    }

    /// Serves until every [`Chats`] is dropped; the agents stop with it.
    pub(crate) async fn run(mut self) {
        loop {
            let key = tokio::select! {
                ask = self.asks.recv() => match ask {
                    Some(Ask::Command(command, answer)) => {
                        let key = (command.workspace.clone(), command.chat.clone());
                        drop(answer.send(self.command(&key, command).await));
                        key
                    }
                    Some(Ask::Query(query, client, answer)) => {
                        let key = (query.workspace.clone(), query.chat.clone());
                        drop(answer.send(self.query(&key, query.query, &client).await));
                        key
                    }
                    None => return,
                },
                Some(report) = self.reports.recv() => {
                    let key = (report.of.workspace, report.of.chat);
                    if let Err(fault) = self.heard(&key, &report.of.run, report.heard).await {
                        let fields = [
                            ("run_id", report.of.run.as_str().into()),
                            ("err", Field::error(&fault)),
                        ];
                        self.log.error("run event not committed", &fields);
                    }
                    key
                }
            };
            self.settle(&key);
        }
    }

    async fn state(&mut self, key: &Key, run: &RunId, state: RunState) -> Result<(), Fault> {
        self.runs
            .update(&key.0, &key.1, |record| record.state = state)
            .await?;
        let status = RunStatus {
            run: run.clone(),
            state,
        };
        self.commit(key, ChatEvent::RunStatus { status })
            .await
            .map(drop)
    }

    async fn receipt(
        &mut self,
        key: &Key,
        request: &RequestId,
        outcome: Outcome,
    ) -> Result<(), Fault> {
        // A command's end is told once, whichever path gets there first.
        if self.chat(key).await?.log.has_ended(request) {
            return Ok(());
        }
        let receipt = Receipt {
            request: request.clone(),
            outcome,
        };
        self.commit(key, ChatEvent::Receipt { receipt })
            .await
            .map(drop)
    }

    // Commits `event` to the chat's log, then tells its subscribers.
    async fn commit(&mut self, key: &Key, event: ChatEvent) -> Result<ChatEntry, Fault> {
        let chat = self.chat(key).await?;
        match chat.log.append(event).await {
            Ok(entry) => {
                self.publish(key, &update(key, entry.clone()));
                Ok(entry)
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
    async fn mend(&mut self, key: &Key) {
        let root = self.workspaces.root(&key.0);
        let Some(chat) = self.chats.get_mut(key) else {
            return;
        };
        for subscriber in chat.subscribers.drain(..) {
            subscriber.end();
        }
        if let Ok(log) = Log::open(&root, &key.1).await {
            chat.log = log;
        }
    }

    fn publish(&mut self, key: &Key, message: &Message) {
        if let Some(chat) = self.chats.get_mut(key) {
            chat.subscribers = std::mem::take(&mut chat.subscribers)
                .into_iter()
                .filter_map(|subscriber| subscriber.tell(message.clone()))
                .collect();
        }
    }

    fn order(&self, key: &Key, order: Order) {
        if let Some(orders) = self.chats.get(key).and_then(|chat| chat.orders.as_ref()) {
            // A run that is gone says so itself, in its last report.
            drop(orders.send(order));
        }
    }

    // Whether the chat's run hasn't yet reported its exit.
    fn is_live(&self, key: &Key) -> bool {
        self.chats
            .get(key)
            .is_some_and(|chat| chat.orders.is_some())
    }

    // Whether the chat's run still reads orders: one that stopped reading reports its exit next.
    fn takes_orders(&self, key: &Key) -> bool {
        self.chats
            .get(key)
            .and_then(|chat| chat.orders.as_ref())
            .is_some_and(|orders| !orders.is_closed())
    }

    fn turn(&self, key: &Key) -> Option<RequestId> {
        self.runs.of(&key.0, &key.1)?.turn.clone()
    }

    async fn chat(&mut self, key: &Key) -> Result<&mut Chat, Fault> {
        let root = self.workspaces.root(&key.0);
        match self.chats.entry(key.clone()) {
            Entry::Occupied(open) => Ok(open.into_mut()),
            Entry::Vacant(closed) => Ok(closed.insert(Chat {
                log: Log::open(&root, &key.1).await?,
                subscribers: Vec::new(),
                orders: None,
                waiting: Vec::new(),
                unended: None,
                interrupting: None,
            })),
        }
    }

    // Closes a chat nobody is using, so its log doesn't stay open for good.
    fn settle(&mut self, key: &Key) {
        if self.chats.get_mut(key).is_some_and(Chat::is_idle) {
            self.chats.remove(key);
        }
    }

    // Lets go of clients that hung up, and of the chats only they were watching.
    fn sweep(&mut self) {
        self.chats.retain(|_, chat| !chat.is_idle());
    }
}

fn update(key: &Key, entry: ChatEntry) -> Message {
    Message::Chat(ChatUpdate {
        workspace: key.0.clone(),
        chat: key.1.clone(),
        entry,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use weacon_protocol::{
        AgentSession, ChatAction, ChatAnswerKind, ChatQueryKind, PermissionAnswer, RunEvent,
    };

    use super::*;
    use crate::chats::test_support::{
        Bench, Started, Told, bench, chat, client, command, prompt, pushed, query, run, soon,
        workspace,
    };

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn session() -> AgentSession {
        AgentSession::try_from(SESSION.to_owned()).unwrap()
    }

    fn text(text: &str) -> RunEvent {
        RunEvent::Text {
            text: text.to_owned(),
        }
    }

    fn done(outcome: Outcome) -> RunEvent {
        RunEvent::Finished { outcome }
    }

    fn asks(run_id: &str, prompt: u32) -> RunEvent {
        RunEvent::PermissionAsked {
            run: run(run_id),
            prompt,
            tool: "Bash".to_owned(),
            input: serde_json::json!({ "command": "ls" }),
        }
    }

    fn allow(request: &str, chat_id: &str, run_id: &str, prompt: u32) -> ChatCommand {
        let answer = PermissionAnswer {
            run: run(run_id),
            prompt,
            allow: true,
        };
        command(request, chat_id, ChatAction::Permission { answer })
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
        bench.chats.command(prompt("r-1", "Hi")).await.unwrap();
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
    async fn a_prompt_starts_a_run_and_its_turn_is_committed_then_published_in_order() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;

        let mut agent = first_turn(&mut bench, &mut pushes).await;
        assert_eq!(
            agent.start,
            Start {
                workspace: workspace(),
                chat: chat("c-1"),
                agent: AgentKind::Claude,
                run: run("run-1"),
                session: None,
            }
        );
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

        bench.chats.command(prompt("r-2", "More")).await.unwrap();
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

        bench
            .chats
            .command(prompt("r-2", "Then this"))
            .await
            .unwrap();
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
    async fn a_repeated_request_does_nothing_and_a_conflicting_or_empty_one_is_refused() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        let mut agent = first_turn(&mut bench, &mut pushes).await;

        bench.chats.command(prompt("r-1", "Hi")).await.unwrap();
        let conflict = bench.chats.command(prompt("r-1", "Bye")).await;
        let empty = bench.chats.command(prompt("r-2", " \n")).await;
        bench.chats.command(prompt("r-3", "Next")).await.unwrap();

        assert_eq!(code_of(conflict), "request.conflict");
        assert_eq!(code_of(empty), "chat.empty_prompt");
        assert_eq!(pushed(&mut pushes, 1).await, ["5 prompt r-3 Next"]);
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
            assert_eq!(
                code_of(bench.chats.command(wrong).await),
                "agent.no_such_prompt"
            );
        }
        agent.told.try_recv().unwrap_err();

        bench
            .chats
            .command(allow("r-2", "c-1", "run-1", 1))
            .await
            .unwrap();
        assert_eq!(
            pushed(&mut pushes, 2).await,
            ["8 receipt r-2 completed", "9 run-1 is Running"]
        );
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.allow)
        );
        let again = bench.chats.command(allow("r-3", "c-1", "run-1", 1)).await;
        assert_eq!(code_of(again), "agent.no_such_prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_cut_off_with_its_run_is_never_answered_in_the_chats_next_run() {
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
        bench.chats.command(prompt("r-4", "Again")).await.unwrap();
        let mut next = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            (next.start.run.as_str(), next.start.session.clone()),
            ("run-2", Some(session()))
        );
        assert_eq!(
            soon(next.told.recv()).await,
            Some(Told::Send("Again".to_owned()))
        );
        let late = bench.chats.command(allow("r-5", "c-1", "run-1", 2)).await;
        assert_eq!(code_of(late), "agent.no_such_prompt");
        next.told.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawn_prompt_stops_waiting_and_an_interrupt_ends_the_turn() {
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
        let late = bench.chats.command(allow("r-2", "c-1", "run-1", 1)).await;
        assert_eq!(code_of(late), "agent.no_such_prompt");

        let interrupt = command("r-3", "c-1", ChatAction::Interrupt);
        bench.chats.command(interrupt).await.unwrap();
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
        bench.chats.command(idle).await.unwrap();
        assert_eq!(pushed(&mut pushes, 1).await, ["13 receipt r-4 completed"]);
        agent.told.try_recv().unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_that_cant_start_fails_the_prompt_and_the_next_one_tries_again() {
        let mut bench = bench();
        let mut pushes = watch(&bench).await;
        bench.broken.store(true, Ordering::SeqCst);

        bench.chats.command(prompt("r-1", "Hi")).await.unwrap();
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
        bench.chats.command(prompt("r-2", "Again")).await.unwrap();

        // The failed prompt left the queue: the new run gets only the next one.
        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(started.start.run, run("run-2"));
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
        bench.chats.command(prompt("r-2", "Second")).await.unwrap();
        bench.chats.command(prompt("r-3", "Third")).await.unwrap();
        assert_eq!(pushed(&mut pushes, 4).await.len(), 4);

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
        let resume = query("c-1", ChatQueryKind::Subscribe { after: 4 });
        assert_eq!(bench.chats.query(resume, &slow).await.unwrap(), None);
        for said in ["One", "Two", "Three"] {
            agent.say.send(text(said)).unwrap();
        }
        assert_eq!(pushed(&mut pushes, 3).await.len(), 3);
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
        assert_eq!(pushed(&mut pushes, 2).await.len(), 2);

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
        assert_eq!(pushed(&mut pushes, 4).await.len(), 4);

        let first = allow("r-2", "c-1", "run-1", 1);
        bench.chats.command(first).await.unwrap();
        let second = allow("r-3", "c-1", "run-1", 2);
        bench.chats.command(second).await.unwrap();

        assert_eq!(
            pushed(&mut pushes, 3).await,
            [
                "9 receipt r-2 completed",
                "10 receipt r-3 completed",
                "11 run-1 is Running",
            ]
        );
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.prompt == 1)
        );
        assert!(
            matches!(soon(agent.told.recv()).await, Some(Told::Answer(answer)) if answer.prompt == 2)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawn_prompt_leaves_the_run_waiting_while_another_waits() {
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

        assert_eq!(pushed(&mut once, 4).await.len(), 4);
        once.try_recv().unwrap_err();
        let lost = bench.chats.query(subscribe(), &full).await.unwrap();
        assert_eq!(lost, Some(ChatAnswerKind::EventsLost));
        assert_eq!(pushed(&mut filled, 1).await, ["events lost"]);
    }

    #[tokio::test(start_paused = true)]
    async fn what_this_build_doesnt_know_is_refused_and_a_stopped_engine_says_so() {
        let bench = bench();
        let (anyone, _pushes) = client(2);

        let action = bench
            .chats
            .command(command("r-1", "c-1", ChatAction::Newer));
        assert_eq!(code_of(action.await), "message.unsupported");
        let asked = bench
            .chats
            .query(query("c-1", ChatQueryKind::Newer), &anyone);
        assert_eq!(
            asked.await.unwrap_err().code().as_str(),
            "message.unsupported"
        );

        let (stopped, engine) = crate::chats::test_support::parts();
        drop(engine);
        let command = stopped.chats.command(prompt("r-1", "Hi")).await;
        assert_eq!(code_of(command), "chat.engine_stopped");
        let snapshot = query("c-1", ChatQueryKind::Snapshot);
        let asked = stopped.chats.query(snapshot, &anyone).await;
        assert_eq!(asked.unwrap_err().code().as_str(), "chat.engine_stopped");
    }
}
