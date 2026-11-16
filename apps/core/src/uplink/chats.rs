//! The server's way to this machine's chats: it relays the owner's commands and subscribes to
//! the chats it stores, whose events go up as they are committed.

use std::collections::{HashMap, VecDeque};

use tokio::sync::mpsc;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand, ChatEntry, ChatEvent, ChatQuery,
    ChatQueryKind, ChatUpdate, Code, Envelope, Message, Outcome, Receipt, RequestId, RunEvent,
    code,
};

mod live;

pub(super) use live::LIVE;

use self::live::Live;
use super::{QUEUE, Step, failure, refusal, reply};
use crate::chats::Chats;
use crate::chats::feed::{self, Client};
use crate::runs::ChatKey;

/// What a server offers when it stores the events of the chats it subscribes to.
pub(super) const UPLOAD: Code = code!("chat.upload");
/// The most events asked for at once to catch up a chat too far behind to replay.
const PAGE: u32 = 256;
/// The most chats one connection follows. Each keeps a place in the link's queue, so the
/// queue holds this many more than it ever waits on.
const FOLLOWS_MAX: usize = 512;

/// What the server asked of the chats, taken up in order.
#[derive(Debug, PartialEq)]
enum Ask {
    /// A command, under the envelope id it came in.
    Command(u32, Box<ChatCommand>),
    /// Follow the chat after the seq given.
    Follow(ChatKey, u64),
}

/// One connection's way to the chats. Dropped with its connection, which ends its
/// subscriptions.
pub(super) struct ChatLink {
    asks: mpsc::Sender<Ask>,
    pushes: mpsc::Receiver<Message>,
    /// The answers to commands taken up, under their envelopes' ids.
    answers: mpsc::Receiver<Envelope>,
    uploads: bool,
    /// Asks waiting for room in `asks`, in the order asked.
    waiting: VecDeque<Ask>,
    /// The seq of the last event of each followed chat that came, to follow again after.
    came: HashMap<ChatKey, u64>,
    live: Live,
}

impl ChatLink {
    /// Takes up asks for `chats` in order; `uploads` when the server offered [`UPLOAD`], and
    /// `live` when it offered [`LIVE`].
    pub(super) fn new(chats: Chats, uploads: bool, live: bool, log: Logger) -> Self {
        let (asks, asked) = mpsc::channel(QUEUE);
        let (push, pushes) = mpsc::channel(feed::QUEUE + FOLLOWS_MAX);
        let (answer, answers) = mpsc::channel(QUEUE);
        tokio::spawn(take_up(chats, Client { push }, asked, answer, log));
        Self {
            asks,
            pushes,
            answers,
            uploads,
            waiting: VecDeque::new(),
            came: HashMap::new(),
            live: Live::new(live),
        }
    }

    pub(super) const fn is_for(message: &Message) -> bool {
        matches!(message, Message::ChatCommand(_) | Message::ChatQuery(_))
    }

    /// Passes on a chat command or a subscription, or refuses it.
    pub(super) fn take(&mut self, envelope: Envelope) -> Step {
        let Envelope { id, message } = envelope;
        if let Message::ChatCommand(command) = message {
            // Only a client on the core's own socket may hold a chat.
            if matches!(command.action, ChatAction::Hold | ChatAction::Release) {
                return reply(refusal(id, &command, code!("message.unsupported")));
            }
            self.queue(Ask::Command(id, Box::new(command)));
            return Step::Nothing;
        }
        if let Message::ChatQuery(ChatQuery {
            workspace,
            chat,
            query: ChatQueryKind::Subscribe { after },
        }) = message
            && self.uploads
        {
            let key = ChatKey { workspace, chat };
            if !self.came.contains_key(&key) && self.came.len() >= FOLLOWS_MAX {
                return reply(failure(id, code!("chat.follows_full")));
            }
            self.came.insert(key.clone(), after);
            self.queue(Ask::Follow(key, after));
            return Step::Nothing;
        }
        reply(failure(id, code!("message.unsupported")))
    }

    // Passes `ask` on in the order asked: at once when nothing waits and there is room, else
    // after what waits. A chat followed again while its follow waits keeps that one's place.
    fn queue(&mut self, ask: Ask) {
        if let Ask::Follow(key, after) = &ask
            && let Some(Ask::Follow(_, waiting)) = self
                .waiting
                .iter_mut()
                .find(|waiting| matches!(waiting, Ask::Follow(other, _) if other == key))
        {
            *waiting = *after;
            return;
        }
        let ask = if self.waiting.is_empty() {
            let Err(full) = self.asks.try_send(ask) else {
                return;
            };
            full.into_inner()
        } else {
            ask
        };
        self.waiting.push_back(ask);
    }

    /// The next event to send up, passing on waiting asks as room frees meanwhile; safe to
    /// cancel, since an ask leaves `waiting` only as it is sent.
    pub(super) async fn next(&mut self) -> Option<Envelope> {
        loop {
            tokio::select! {
                pushed = self.pushes.recv() => {
                    if let Some(up) = self.upload(pushed?) {
                        return Some(up);
                    }
                }
                Some(answer) = self.answers.recv() => return Some(answer),
                room = self.asks.clone().reserve_owned(), if !self.waiting.is_empty() => {
                    let Ok(room) = room else {
                        // Nothing takes asks any more, so none can wait for room.
                        self.waiting.clear();
                        continue;
                    };
                    if let Some(ask) = self.waiting.pop_front() {
                        room.send(ask);
                    }
                }
            }
        }
    }

    /// What of `pushed` goes up: a chat's events as the server stores them, and its runs' live
    /// updates.
    /// A chat whose events were lost is followed again after the last that came.
    pub(super) fn upload(&mut self, pushed: Message) -> Option<Envelope> {
        if let Message::Chat(update) = pushed {
            let key = ChatKey::new(&update.workspace, &update.chat);
            self.came.insert(key, update.entry.seq);
            self.live.saw(&update);
            let message = Message::Chat(stored(update)?);
            return Some(Envelope { id: 0, message });
        }
        if let Message::Run(update) = pushed {
            return self.live.of(update, &self.came);
        }
        if let Message::ChatAnswer(ChatAnswer {
            workspace,
            chat,
            answer: ChatAnswerKind::EventsLost,
        }) = pushed
        {
            let key = ChatKey { workspace, chat };
            let after = *self.came.get(&key)?;
            self.queue(Ask::Follow(key, after));
        }
        None
    }
}

/// `update` as the server keeps it: never a tool's input, only what a prompt says it wants;
/// nor a run's start, its tool calls or text still being written.
fn stored(mut update: ChatUpdate) -> Option<ChatUpdate> {
    match &mut update.entry.event {
        ChatEvent::Run { update: run } => match &mut run.event {
            RunEvent::PermissionAsked { input, .. } => *input = serde_json::Value::Null,
            RunEvent::Text { .. }
            | RunEvent::PermissionWithdrawn { .. }
            | RunEvent::PermissionAnswered { .. }
            | RunEvent::Finished { .. } => {}
            RunEvent::Started { .. }
            | RunEvent::TextDelta { .. }
            | RunEvent::ToolCall { .. }
            | RunEvent::ToolDone { .. } => return None,
        },
        ChatEvent::Prompt { .. }
        | ChatEvent::OutsidePrompt { .. }
        | ChatEvent::Receipt { .. }
        | ChatEvent::RunStatus { .. } => {}
        ChatEvent::Message { .. } => return None,
    }
    Some(update)
}

// Takes up the server's asks in the order they came, until the connection is gone, and answers
// each command, taken or refused.
async fn take_up(
    chats: Chats,
    client: Client,
    mut asked: mpsc::Receiver<Ask>,
    answer: mpsc::Sender<Envelope>,
    log: Logger,
) {
    let mut jumps = Jumps::new();
    while let Some(ask) = asked.recv().await
        && !client.is_gone()
    {
        let (key, request, taken) = match ask {
            Ask::Command(id, command) => {
                let key = ChatKey::new(&command.workspace, &command.chat);
                let request = command.request.clone();
                let taken = chats.command(*command, &client).await;
                drop(answer.send(answered(id, request.clone(), &taken)).await);
                (key, Some(request), taken)
            }
            Ask::Follow(key, after) => {
                let followed = follow(&chats, &client, &key, after, &mut jumps).await;
                (key, None, followed)
            }
        };
        if let Err(fault) = taken {
            let mut fields = vec![
                ("workspace_id", key.workspace.as_str().into()),
                ("chat_id", key.chat.as_str().into()),
                ("err", Field::error(&fault)),
            ];
            fields.extend(request.map(|request| ("request_id", request.as_str().into())));
            log.warn("chat ask refused", &fields);
        }
    }
}

// The answer to `request`'s command under the envelope `id`: taken, or the error that refused it.
fn answered(id: u32, request: RequestId, taken: &Result<(), Fault>) -> Envelope {
    match taken {
        Ok(()) => Envelope {
            id,
            message: Message::Receipt(Receipt {
                request,
                outcome: Outcome::Accepted,
            }),
        },
        Err(fault) => failure(id, fault.code().clone()),
    }
}

// Follows `key`'s chat after `after`. When that is too much to send again at once, what is
// missing goes up a page at a time first; past the log's end, it follows from there.
async fn follow(
    chats: &Chats,
    client: &Client,
    key: &ChatKey,
    asked_after: u64,
    jumps: &mut Jumps,
) -> Result<(), Fault> {
    // The link knows only the seq the server asked after, so a chat that jumped from it goes on
    // from where it landed, or what came since would be jumped over too.
    let mut after = match jumps.get(key) {
        Some(&(from, to)) if from == asked_after => to,
        _ => asked_after,
    };
    loop {
        let start = after;
        // A follow can't start without room for its client to be told it missed events.
        let Ok(room) = client.push.reserve().await else {
            return Ok(());
        };
        drop(room);
        let follow = asked(key, ChatQueryKind::Subscribe { after });
        if chats.query(follow, client).await? != Some(ChatAnswerKind::EventsLost) {
            return Ok(());
        }
        let page = page(chats, client, key, after).await?;
        let last = page.last().map_or(0, |entry| entry.seq);
        if last <= after {
            jumps.insert(key.clone(), (asked_after, last));
            after = last;
        }
        for entry in newer(page, after) {
            after = entry.seq;
            let update = ChatUpdate {
                workspace: key.workspace.clone(),
                chat: key.chat.clone(),
                entry,
            };
            if client.push.send(Message::Chat(update)).await.is_err() {
                return Ok(());
            }
        }
        // Each round sends something up or moves to the log's end; one that can't is given up.
        if after == start {
            return Err(Fault::new(
                code!("chat.not_followed"),
                "The chat couldn't be followed",
            ));
        }
    }
}

/// Where a chat followed past its log's end jumped: from the seq asked after, to the log's end.
type Jumps = HashMap<ChatKey, (u64, u64)>;

// The events right after `after`, as many as one answer holds; past the log's end, its last.
async fn page(
    chats: &Chats,
    client: &Client,
    key: &ChatKey,
    after: u64,
) -> Result<Vec<ChatEntry>, Fault> {
    let mut limit = PAGE;
    loop {
        let before = after.saturating_add(u64::from(limit)).saturating_add(1);
        let window = asked(key, ChatQueryKind::Window { before, limit });
        let Some(ChatAnswerKind::Window { events }) = chats.query(window, client).await? else {
            return Ok(Vec::new());
        };
        // A window keeps the latest events that fit, so a full one may start too late; one
        // event always fits, and none is asked for at all once `limit` reaches 0.
        let next = after.saturating_add(1);
        if events.first().is_none_or(|first| first.seq <= next) {
            return Ok(events);
        }
        limit /= 2;
    }
}

// The entries of `page` after `after`: a page that reaches the log's end may hold earlier ones.
fn newer(page: Vec<ChatEntry>, after: u64) -> impl Iterator<Item = ChatEntry> {
    page.into_iter().filter(move |entry| entry.seq > after)
}

fn asked(key: &ChatKey, query: ChatQueryKind) -> ChatQuery {
    ChatQuery {
        workspace: key.workspace.clone(),
        chat: key.chat.clone(),
        query,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use weacon_protocol::{Outcome, Receipt, RunUpdate, Wants};
    use weacon_testing::{Records, recording_logger};

    use super::*;
    use crate::chats::test_support::{
        bench, chat, command, key, logged_prompt, parts, prompt, prompt_in, query, request, run,
        session, someone, soon, text, workspace,
    };

    // A link with no worker, whose asks wait in a queue with room for `room` for the test to
    // read, and the way to push to it.
    fn link(uploads: bool, room: usize) -> (ChatLink, mpsc::Receiver<Ask>, Client) {
        let (asks, asked) = mpsc::channel(room);
        let (push, pushes) = mpsc::channel(4);
        let link = ChatLink {
            asks,
            pushes,
            answers: mpsc::channel(1).1,
            uploads,
            waiting: VecDeque::new(),
            came: HashMap::new(),
            live: Live::new(true),
        };
        (link, asked, Client { push })
    }

    fn envelope(message: Message) -> Envelope {
        Envelope { id: 9, message }
    }

    // `message` as the core sends it up.
    fn up(message: Message) -> Envelope {
        Envelope { id: 0, message }
    }

    fn subscribe(after: u64) -> Envelope {
        envelope(Message::ChatQuery(query(
            "c-1",
            ChatQueryKind::Subscribe { after },
        )))
    }

    fn told(request: &str, text: &str) -> Envelope {
        envelope(Message::ChatCommand(prompt(request, text)))
    }

    fn update(seq: u64, event: ChatEvent) -> Message {
        Message::Chat(ChatUpdate {
            workspace: workspace(),
            chat: chat("c-1"),
            entry: ChatEntry { seq, event },
        })
    }

    fn said(event: RunEvent) -> ChatEvent {
        let update = RunUpdate {
            run: run("run-1"),
            event,
        };
        ChatEvent::Run { update }
    }

    fn lost() -> Message {
        Message::ChatAnswer(ChatAnswer {
            workspace: workspace(),
            chat: chat("c-1"),
            answer: ChatAnswerKind::EventsLost,
        })
    }

    fn error_code(step: &Step) -> Option<&str> {
        let Step::Reply(reply) = step else {
            return None;
        };
        if let Message::Error(error) = &reply.message {
            return Some(error.code.as_str());
        }
        let Message::Receipt(Receipt {
            outcome: Outcome::Failed { code },
            ..
        }) = &reply.message
        else {
            return None;
        };
        Some(code.as_str())
    }

    // What goes up next within a few seconds, past what doesn't go up.
    async fn next_up(link: &mut ChatLink) -> Option<Envelope> {
        tokio::time::timeout(Duration::from_secs(5), link.next())
            .await
            .ok()
            .flatten()
    }

    // The next uploaded event's seq and, for a prompt, its request.
    async fn next_seq(link: &mut ChatLink) -> (u64, Option<String>) {
        let Some(Envelope {
            message: Message::Chat(update),
            ..
        }) = next_up(link).await
        else {
            panic!("nothing went up");
        };
        let ChatEvent::Prompt { request, .. } = update.entry.event else {
            return (update.entry.seq, None);
        };
        let asked = Some(request.as_str().to_owned());
        (update.entry.seq, asked)
    }

    // The seqs that go up until the next prompt, and that prompt's request.
    async fn until_prompt(link: &mut ChatLink) -> (Vec<u64>, String) {
        let mut seqs = Vec::new();
        loop {
            let (seq, asked) = next_seq(link).await;
            seqs.push(seq);
            if let Some(asked) = asked {
                return (seqs, asked);
            }
        }
    }

    #[test]
    fn a_servers_follows_and_commands_pass_in_the_order_asked() {
        let (mut link, mut asked, _push) = link(true, 4);

        let followed = link.take(subscribe(7));
        let commanded = link.take(told("r-1", "Hi"));

        assert!(matches!(followed, Step::Nothing));
        assert!(matches!(commanded, Step::Nothing));
        assert_eq!(asked.try_recv().unwrap(), Ask::Follow(key(), 7));
        assert_eq!(
            asked.try_recv().unwrap(),
            Ask::Command(9, Box::new(prompt("r-1", "Hi")))
        );
        assert_eq!(link.came.get(&key()), Some(&7));
    }

    // Takes from `asked` until it is empty.
    fn drained(asked: &mut mpsc::Receiver<Ask>) -> Vec<Ask> {
        std::iter::from_fn(|| asked.try_recv().ok()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn what_waits_for_room_keeps_its_order_and_a_chat_followed_again_its_place() {
        let (mut link, mut asked, _push) = link(true, 1);
        drop(link.take(told("r-1", "Hi")));
        drop(link.take(subscribe(100)));
        drop(link.take(told("r-2", "Hi")));
        drop(link.take(subscribe(3)));

        let mut asks = Vec::new();
        for _ in 0..3 {
            drop(next_up(&mut link).await);
            asks.extend(drained(&mut asked));
        }

        let expected = [
            Ask::Command(9, Box::new(prompt("r-1", "Hi"))),
            Ask::Follow(key(), 3),
            Ask::Command(9, Box::new(prompt("r-2", "Hi"))),
        ];
        assert_eq!(asks, expected);
        assert!(link.waiting.is_empty());
    }

    #[test]
    fn a_connection_follows_a_bounded_number_of_chats() {
        let (mut link, _asked, _push) = link(true, 1);
        let follow = |id: &str| {
            envelope(Message::ChatQuery(query(
                id,
                ChatQueryKind::Subscribe { after: 0 },
            )))
        };
        let taken: Vec<_> = (0..FOLLOWS_MAX)
            .map(|at| link.take(follow(&format!("c-{at}"))))
            .collect();

        let one_more = link.take(follow("c-new"));
        let again = link.take(follow("c-0"));

        assert!(taken.iter().all(|step| matches!(step, Step::Nothing)));
        assert_eq!(error_code(&one_more), Some("chat.follows_full"));
        assert!(matches!(again, Step::Nothing));
    }

    #[tokio::test(start_paused = true)]
    async fn a_follow_with_no_room_yet_is_passed_on_once_there_is() {
        let (mut link, mut asked, _push) = link(true, 1);
        drop(link.take(told("r-1", "Hi")));
        drop(link.take(subscribe(3)));

        let while_full = next_up(&mut link).await;
        let first = soon(asked.recv()).await.unwrap();
        let once_room = next_up(&mut link).await;

        assert_eq!((while_full, once_room), (None, None));
        assert!(matches!(first, Ask::Command(..)));
        assert_eq!(asked.try_recv().unwrap(), Ask::Follow(key(), 3));
        assert!(link.waiting.is_empty());
    }

    #[test]
    fn only_a_server_that_stores_events_may_follow_and_it_may_ask_nothing_else() {
        let (mut uploading, mut asked, _push) = link(true, 4);
        let (mut not_uploading, _not_asked, _not_pushed) = link(false, 4);

        let snapshot = Message::ChatQuery(query("c-1", ChatQueryKind::Snapshot));
        let refused = [
            not_uploading.take(subscribe(0)),
            uploading.take(envelope(snapshot)),
        ];

        let codes = refused.iter().map(error_code).collect::<Vec<_>>();
        assert_eq!(codes, [Some("message.unsupported"); 2]);
        asked.try_recv().unwrap_err();
        assert!(not_uploading.waiting.is_empty() && uploading.waiting.is_empty());
        assert!(not_uploading.came.is_empty());
    }

    #[test]
    fn a_page_gives_only_what_came_after() {
        let page = [3, 4, 5].map(|seq| ChatEntry {
            seq,
            event: logged_prompt("r-1", "Hi"),
        });

        let seqs: Vec<_> = newer(page.to_vec(), 4).map(|entry| entry.seq).collect();

        assert_eq!(seqs, [5]);
    }

    #[test]
    fn no_hold_is_taken_from_the_server() {
        let (mut link, mut asked, _push) = link(true, 4);

        let refused = [ChatAction::Hold, ChatAction::Release].map(|action| {
            link.take(envelope(Message::ChatCommand(command(
                "r-1", "c-1", action,
            ))))
        });

        let codes = refused.iter().map(error_code).collect::<Vec<_>>();
        assert_eq!(codes, [Some("message.unsupported"); 2]);
        asked.try_recv().unwrap_err();
    }

    #[test]
    fn commands_with_no_room_yet_all_wait_for_it() {
        let (mut link, _asked, _push) = link(true, 1);
        let interrupt = |at: usize| command(&format!("r-{at}"), "c-1", ChatAction::Interrupt);

        let taken: Vec<_> = (0..QUEUE + 2)
            .map(|at| link.take(envelope(Message::ChatCommand(interrupt(at)))))
            .collect();

        assert!(taken.iter().all(|step| matches!(step, Step::Nothing)));
        // One went to the engine's queue of one; the rest wait.
        assert_eq!(link.waiting.len(), QUEUE + 1);
    }

    // Events the server keeps, as they are.
    fn kept() -> [ChatEvent; 6] {
        let receipt = ChatEvent::Receipt {
            receipt: Receipt {
                request: request("r-1"),
                outcome: Outcome::Accepted,
            },
        };
        [
            logged_prompt("r-1", "MARKER-PROMPT"),
            ChatEvent::OutsidePrompt {
                text: "Hi".to_owned(),
            },
            receipt,
            said(text("MARKER-TEXT")),
            said(RunEvent::PermissionWithdrawn {
                run: run("run-1"),
                prompt: 1,
            }),
            said(RunEvent::Finished {
                outcome: Outcome::Completed,
            }),
        ]
    }

    // Events the server never keeps: none goes up.
    fn dropped() -> [ChatEvent; 4] {
        [
            said(RunEvent::Started { session: session() }),
            said(RunEvent::TextDelta {
                text: "MARKER".to_owned(),
            }),
            said(RunEvent::ToolCall {
                call: "t-1".to_owned(),
                tool: "Bash".to_owned(),
                input: json!({ "command": "MARKER-INPUT" }),
            }),
            said(RunEvent::ToolDone {
                call: "t-1".to_owned(),
                failed: false,
            }),
        ]
    }

    #[test]
    fn messages_go_up_as_the_server_keeps_them_and_nothing_else_does() {
        let (mut link, _asked, _push) = link(true, 4);
        let ask = |input: Value| RunEvent::PermissionAsked {
            run: run("run-1"),
            prompt: 1,
            tool: "Bash".to_owned(),
            input,
            wants: Some(Wants::of("cargo test")),
        };
        let (kept, dropped) = (kept(), dropped());
        let mut send = |seq, event| link.upload(update(seq, event));
        let ups: Vec<_> = kept.iter().map(|event| send(1, event.clone())).collect();
        let downs: Vec<_> = dropped.into_iter().map(|event| send(2, event)).collect();
        let stripped = send(3, said(ask(json!({ "command": "MARKER" }))));
        let live = link.upload(Message::Run(RunUpdate {
            run: run("run-1"),
            event: text("Hi"),
        }));

        let expected: Vec<_> = kept
            .into_iter()
            .map(|event| Some(up(update(1, event))))
            .collect();
        assert_eq!(ups, expected);
        assert!(downs.iter().all(Option::is_none), "{downs:?}");
        // Its tool and what it wants go up, and never its input.
        assert_eq!(stripped, Some(up(update(3, said(ask(Value::Null))))));
        assert_eq!(live, None);
        assert_eq!(link.came.get(&key()), Some(&3));
    }

    #[test]
    fn a_chat_whose_events_were_lost_is_followed_again_after_the_last_that_came() {
        let (mut link, mut asked, _push) = link(true, 4);
        drop(link.take(subscribe(2)));

        let first = link.upload(lost());
        let started = said(RunEvent::Started { session: session() });
        drop(link.upload(update(5, started)));
        let again = link.upload(lost());

        assert_eq!((first, again), (None, None));
        let follows = [2, 2, 5].map(|after| Ask::Follow(key(), after));
        assert_eq!(drained(&mut asked), follows);
    }

    #[test]
    fn events_lost_from_a_chat_never_followed_ask_nothing() {
        let (mut link, _asked, _push) = link(true, 4);

        assert_eq!(link.upload(lost()), None);
        assert!(link.waiting.is_empty());
    }

    // Commits `count` prompts of `size` bytes each to chat `c-1`, under r-0, r-1 and so on.
    async fn prompts(chats: &Chats, count: usize, size: usize) {
        for at in 0..count {
            let asked = prompt(&format!("r-{at}"), &"a".repeat(size));
            chats.command(asked, &someone()).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_too_far_behind_to_send_again_at_once_goes_up_a_page_at_a_time() {
        let bench = bench();
        prompts(&bench.chats, 10, 900_000).await;
        let mut link = ChatLink::new(bench.chats.clone(), true, true, recording_logger().0);

        drop(link.take(subscribe(0)));
        let mut prompted = Vec::new();
        let mut seqs = Vec::new();
        while prompted.len() < 10 {
            let (seq, asked) = next_seq(&mut link).await;
            seqs.push(seq);
            prompted.extend(asked);
        }
        bench.command(prompt("r-new", "Hi")).await.unwrap();
        let (later, newer) = until_prompt(&mut link).await;
        seqs.extend(later);

        let expected: Vec<_> = (0..10).map(|at| format!("r-{at}")).collect();
        assert_eq!(prompted, expected);
        assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
        assert_eq!(seqs.first(), Some(&1));
        assert_eq!(newer, "r-new");
    }

    #[tokio::test]
    async fn a_command_the_chats_refuse_is_answered_under_its_envelope() {
        let bench = bench();
        let mut link = ChatLink::new(bench.chats.clone(), true, true, recording_logger().0);
        let unknown = weacon_protocol::RepoId::try_from("typo".to_owned()).unwrap();
        let asked = prompt_in("r-1", "Hi", &unknown);

        drop(link.take(envelope(Message::ChatCommand(asked))));

        let refused = failure(9, code!("repo.unknown"));
        assert_eq!(next_up(&mut link).await, Some(refused));
    }

    #[test]
    fn a_command_taken_is_answered_accepted_and_one_refused_with_its_code() {
        let taken = answered(3, request("r-1"), &Ok(()));
        let accepted = Message::Receipt(Receipt {
            request: request("r-1"),
            outcome: Outcome::Accepted,
        });
        assert_eq!(
            taken,
            Envelope {
                id: 3,
                message: accepted
            }
        );
        let busy = Err(Fault::new(code!("chat.busy"), "Busy"));
        assert_eq!(
            answered(3, request("r-1"), &busy),
            failure(3, code!("chat.busy"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_follow_past_the_logs_end_goes_on_from_there() {
        let bench = bench();
        prompts(&bench.chats, 1, 2).await;
        let mut link = ChatLink::new(bench.chats.clone(), true, true, recording_logger().0);

        drop(link.take(subscribe(100)));
        // Passes the follow on, and gives what went up meanwhile, if anything did.
        let first = next_up(&mut link).await;
        bench.command(prompt("r-new", "Hi")).await.unwrap();
        let (mut seqs, newer) = until_prompt(&mut link).await;
        if let Some(Envelope {
            message: Message::Chat(update),
            ..
        }) = first
        {
            seqs.push(update.entry.seq);
        }

        // The prompt and receipt already logged don't go up again.
        assert!(seqs.iter().all(|seq| *seq > 2), "{seqs:?}");
        assert_eq!(newer, "r-new");
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_followed_again_from_where_it_jumped_misses_nothing_since() {
        let bench = bench();
        prompts(&bench.chats, 1, 2).await;
        let mut jumps = Jumps::new();
        let (first, first_pushes) = crate::chats::test_support::client(16);
        follow(&bench.chats, &first, &key(), 100, &mut jumps)
            .await
            .unwrap();
        // The first subscription is gone before the chat goes on, as one that lost events is.
        drop(first_pushes);
        bench.command(prompt("r-new", "Hi")).await.unwrap();

        let (again, mut pushes) = crate::chats::test_support::client(16);
        follow(&bench.chats, &again, &key(), 100, &mut jumps)
            .await
            .unwrap();

        let prompted = std::iter::from_fn(|| pushes.try_recv().ok()).any(|pushed| {
            matches!(pushed, Message::Chat(ChatUpdate { entry: ChatEntry { event: ChatEvent::Prompt { request, .. }, .. }, .. }) if request.as_str() == "r-new")
        });
        assert!(prompted, "the prompt made after the jump never came");

        // A follow from any other seq is the server's own, and starts there.
        let (other, mut from_start) = crate::chats::test_support::client(16);
        follow(&bench.chats, &other, &key(), 0, &mut jumps)
            .await
            .unwrap();
        let first = from_start.try_recv().ok();
        assert!(
            matches!(
                first,
                Some(Message::Chat(ChatUpdate {
                    entry: ChatEntry { seq: 1, .. },
                    ..
                }))
            ),
            "{first:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn what_a_connection_left_queued_when_it_ended_is_not_done() {
        let mut bench = bench();
        let mut link = ChatLink::new(bench.chats.clone(), true, true, recording_logger().0);

        drop(link.take(told("r-1", "Hi")));
        drop(link);

        assert!(soon(bench.started.recv()).await.is_none());
    }

    // The fields of the first line logged as `msg`.
    async fn logged(records: &Records, msg: &str) -> HashMap<String, Value> {
        for _ in 0..1000 {
            let found = records
                .lock()
                .unwrap()
                .iter()
                .find(|record| record.msg == msg)
                .map(|record| record.fields.clone());
            if let Some(fields) = found {
                return fields.into_iter().collect();
            }
            tokio::task::yield_now().await;
        }
        panic!("nothing logged as {msg}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_ask_the_engine_refuses_is_logged_by_its_ids_alone() {
        let stopped = parts().0.chats;
        let (log, records) = recording_logger();
        let mut link = ChatLink::new(stopped, true, true, log);

        drop(link.take(told("r-1", "MARKER-PROMPT")));
        let fields = logged(&records, "chat ask refused").await;

        let named = ["workspace_id", "chat_id", "request_id"].map(|name| fields.get(name).cloned());
        assert_eq!(
            named,
            [Some(json!("w-1")), Some(json!("c-1")), Some(json!("r-1"))]
        );
        assert!(!format!("{fields:?}").contains("MARKER"));
        drop(link);
    }
}
