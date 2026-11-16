//! Asks one of a person's machines for something over its open connection, and takes the
//! machine's answer back to the connection that asked.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio::time::sleep;
use weacon_observability::Fault;
use weacon_protocol::{
    Asker, Capability, ChatCommand, Code, DiffAnswer, DiffAnswerKind, Envelope, ErrorBody,
    MachineId, Message, code,
};

use crate::db::Workspace;
use crate::shared::Shared;
use crate::socket::refusal;

mod kinds;

pub(crate) use kinds::needs;
use kinds::{About, Asked, answering, for_members, machine_needs, on_machine, stamped};

/// How long an ask waits for its answer, or a diff for its next file.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// What a machine's core offers, and the server offers, to have its settings relayed.
pub(crate) const SETTINGS: Code = code!("machine.settings");
/// What a client and a core offer to have a person's own sessions, run outside Weacon, read
/// through it.
pub(crate) const SESSIONS: Code = code!("sessions.read");
/// What a client and a core offer to have a chat's diff relayed.
pub(crate) const DIFF: Code = code!("diff.view");
/// What a client and a core offer to have a chat's worktree questions and commands relayed.
pub(crate) const WORKTREE: Code = code!("worktree.manage");
/// What a client and a core offer to have a person's search of their own machine's code relayed.
pub(crate) const SYMBOLS: Code = code!("symbols.search");
/// What a core offers where its machine has a desktop terminal.
const TERMINAL: Code = code!("worktree.open_terminal");

/// The asks waiting for their machines' answers.
pub(crate) struct Relay {
    /// Each ask that waits, under the id it was relayed with.
    waiting: Mutex<HashMap<u32, Waiter>>,
    answer_within: Duration,
}

/// An ask's way back to the connection that asked.
struct Waiter {
    sent: Sent,
    id: u32,
    back: mpsc::Sender<Envelope>,
    /// How many of a diff's files are still to come, once its first one says.
    files_left: Option<u32>,
    /// What tells it, if nothing answers it first, that no answer came.
    timer: AbortHandle,
    /// Which of its timers is the live one; one that woke before its restart does nothing.
    round: u32,
}

/// What an ask asked, which its answer must name, and the machine it was sent to, the only one
/// that may answer it.
struct Sent {
    asked: Asked,
    machine: MachineId,
}

impl Waiter {
    // A gone or slow asker misses it; an answer this server can't send on goes as
    // `relay.unsendable`, so the asker's connection goes on. True when `message` itself went on.
    fn tell(&self, message: Message) -> bool {
        let mut envelope = Envelope {
            id: self.id,
            message,
        };
        let sendable = envelope.to_json().is_ok();
        if !sendable {
            envelope.message = refusal(code!("relay.unsendable"));
        }
        self.back.try_send(envelope).is_ok() && sendable
    }

    /// Whether `message` leaves more of a diff to come.
    fn more_after(&mut self, message: &Message) -> bool {
        let Message::DiffAnswer(DiffAnswer {
            answer: DiffAnswerKind::File { files, .. },
            ..
        }) = message
        else {
            return false;
        };
        let left = self.files_left.get_or_insert(*files);
        *left = left.saturating_sub(1);
        *left > 0
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// Who asks, from a signed-in connection to `workspace`.
pub(crate) struct Ask<'a> {
    pub(crate) asker: &'a Asker,
    pub(crate) workspace: &'a Workspace,
}

/// Why a message wasn't relayed.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The error to answer the asker with.
    Refused(ErrorBody),
    /// The relay failed, as a database can.
    Failed(Fault),
}

/// Forwards `envelope` to the machine its message names, or that runs the chat it names, if
/// the machine is the asker's own, the chat theirs too, and a message for a workspace is for
/// the asker's. The answers reach `back` under the envelope's id, `relay.unsendable` if this
/// server can't send one on, or `machine.no_answer` once it has waited too long.
///
/// # Errors
/// `relay.unknown` for a kind the relay doesn't carry or a value it can't send on,
/// `machine.not_found` for a machine that isn't the asker's or another workspace's,
/// `chat.not_found` and `chat.not_owner`, `machine.offline` and `relay.busy`, which may pass,
/// and the database's failure.
pub(crate) async fn forward(
    shared: &Shared,
    ask: &Ask<'_>,
    envelope: Envelope,
    back: &mpsc::Sender<Envelope>,
) -> Result<(), Refusal> {
    let unknown = || refused(code!("relay.unknown"), false);
    let (asked, message) = stamped(envelope.message, ask.asker).ok_or_else(unknown)?;
    let needs = machine_needs(&message).ok_or_else(unknown)?;
    let message = crate::socket::sendable(message).ok_or_else(unknown)?;
    let machine = machine_for(shared, ask, &asked, &message).await?;
    let relayed = shared.links.next_id();
    let sent_to = Sent {
        asked,
        machine: machine.clone(),
    };
    shared.relay.wait(relayed, sent_to, envelope.id, back)?;
    let sent = Envelope {
        id: relayed,
        message,
    };
    if shared
        .links
        .send(&machine, &Capability::new(needs), sent)
        .is_err()
    {
        shared.relay.lock().remove(&relayed);
        return Err(refused(code!("machine.offline"), true));
    }
    Ok(())
}

/// Where an answer goes: the connection's way back, under the envelope id it answers.
#[derive(Clone, Copy)]
pub(crate) struct Back<'a> {
    pub(crate) id: u32,
    pub(crate) to: &'a mpsc::Sender<Envelope>,
}

/// Sends `command` to `machine`, then `follow`, both or neither; the core's answer goes `back`.
///
/// # Errors
/// `relay.busy`, which may pass, and `machine.offline`.
pub(crate) fn command(
    shared: &Shared,
    machine: &MachineId,
    command: ChatCommand,
    follow: Message,
    back: Back<'_>,
) -> Result<(), Fault> {
    let relayed = shared.links.next_id();
    let about = About::Command(command.chat.clone());
    let sent = Sent {
        asked: Asked::new(&command.workspace, &command.request, about),
        machine: machine.clone(),
    };
    let busy = |_| Fault::new(code!("relay.busy"), "Too many asks wait for answers");
    shared
        .relay
        .wait(relayed, sent, back.id, back.to)
        .map_err(busy)?;
    let messages = vec![
        (relayed, Message::ChatCommand(command)),
        (shared.links.next_id(), follow),
    ];
    let sent = crate::chats::send_all(shared, machine, messages);
    if sent.is_err() {
        shared.relay.lock().remove(&relayed);
    }
    sent
}

/// The machine that `message`, which asks `asked`, goes to: the one it names, for a chat one
/// that has an agent there, if it is the asker's own or, for what is [`for_members`], shared
/// with their workspace.
async fn machine_for(
    shared: &Shared,
    ask: &Ask<'_>,
    asked: &Asked,
    message: &Message,
) -> Result<MachineId, Refusal> {
    let elsewhere = asked
        .workspace
        .as_ref()
        .is_some_and(|named| named != ask.workspace.id());
    let machine = match &asked.about {
        About::Machine(_) | About::Symbols(_) if elsewhere => {
            return Err(refused(code!("machine.not_found"), false));
        }
        About::Machine(machine) | About::Sessions(machine) | About::Symbols(machine) => {
            machine.clone()
        }
        About::Diff(_) | About::Worktree(_) | About::Command(_) if elsewhere => {
            return Err(refused(code!("chat.not_found"), false));
        }
        About::Diff(chat) | About::Worktree(chat) | About::Command(chat) => {
            let found = ask.workspace.chat(chat).await.map_err(Refusal::Failed)?;
            found.ok_or_else(|| refused(code!("chat.not_found"), false))?;
            let not_owner = || refused(code!("chat.not_owner"), false);
            let named = on_machine(message).ok_or_else(not_owner)?;
            let leg = ask.workspace.leg(chat, named).await;
            leg.map_err(Refusal::Failed)?.ok_or_else(not_owner)?.machine
        }
    };
    if for_members(message) {
        let callable = ask.workspace.callable(&ask.asker.account, &machine).await;
        let callable = callable.map_err(Refusal::Failed)?;
        callable.ok_or_else(|| refused(code!("machine.not_found"), false))?;
    } else {
        theirs(shared, ask, &machine).await?;
    }
    Ok(machine)
}

impl Default for Relay {
    fn default() -> Self {
        Self::new(ANSWER_WITHIN)
    }
}

impl Relay {
    /// A relay where each ask waits for at most `answer_within`.
    pub(crate) fn new(answer_within: Duration) -> Self {
        Self {
            waiting: Mutex::default(),
            answer_within,
        }
    }

    /// Takes `envelope` from `machine`'s connection: an answer, or an error, to an ask the
    /// relay forwarded to that machine goes back to the connection that asked. A diff's answers
    /// go back until its last file; anything else, or an answer that names another machine, is
    /// dropped. False when its id isn't one of the relay's, so another feature may claim it.
    #[must_use]
    pub(crate) fn answered(self: &Arc<Self>, machine: &MachineId, envelope: Envelope) -> bool {
        let mut waiting = self.lock();
        let others = sharing(&waiting, envelope.id);
        let Entry::Occupied(mut entry) = waiting.entry(envelope.id) else {
            return false;
        };
        let message = envelope.message;
        let named = answering(&message);
        let waiter = entry.get_mut();
        let asked = &waiter.sent.asked;
        let answers = match (&named, &message) {
            (Some(named), _) => named == asked,
            (None, Message::Receipt(receipt)) => {
                matches!(asked.about, About::Command(_)) && receipt.request == asked.request
            }
            (None, message) => matches!(message, Message::Error(_)),
        };
        if waiter.sent.machine != *machine || !answers {
            return true;
        }
        if waiter.more_after(&message) {
            // The asker's other asks keep their room, and the last of the rest goes to telling
            // a slow asker it fell behind.
            if waiter.back.capacity() <= others + 1 {
                entry.remove().tell(Message::Error(ErrorBody {
                    code: code!("relay.busy"),
                    retryable: true,
                }));
            } else if waiter.tell(message) {
                waiter.timer.abort();
                waiter.round = waiter.round.wrapping_add(1);
                waiter.timer = self.timer(envelope.id, waiter.round);
            } else {
                entry.remove();
            }
            return true;
        }
        // Told before the lock goes, as in `expire`.
        entry.remove().tell(message);
        true
    }

    /// Waits, under `relayed`, for the answer to what the asker `sent` under `id`.
    fn wait(
        self: &Arc<Self>,
        relayed: u32,
        sent: Sent,
        id: u32,
        back: &mpsc::Sender<Envelope>,
    ) -> Result<(), Refusal> {
        let mut waiting = self.lock();
        let theirs = waiting
            .values()
            .filter(|waiter| waiter.back.same_channel(back))
            .count();
        // Within the asker's room, so every answer but a diff's later files fits.
        if theirs >= back.capacity() {
            return Err(refused(code!("relay.busy"), true));
        }
        let Entry::Vacant(entry) = waiting.entry(relayed) else {
            return Err(refused(code!("relay.busy"), true));
        };
        entry.insert(Waiter {
            sent,
            id,
            back: back.clone(),
            files_left: None,
            timer: self.timer(relayed, 0),
            round: 0,
        });
        Ok(())
    }

    /// What tells the ask under `relayed`, unless something stops it first, that no answer
    /// came in time for its timer `round`.
    fn timer(self: &Arc<Self>, relayed: u32, round: u32) -> AbortHandle {
        let relay = Arc::clone(self);
        tokio::spawn(async move {
            sleep(relay.answer_within).await;
            relay.expire(relayed, round);
        })
        .abort_handle()
    }

    /// Tells the ask under `relayed`, if it still waits on timer `round`, that no answer came.
    fn expire(&self, relayed: u32, round: u32) {
        let mut waiting = self.lock();
        let Entry::Occupied(entry) = waiting.entry(relayed) else {
            return;
        };
        if entry.get().round != round {
            return;
        }
        // Told before the lock goes, so no ask is let in ahead of the answer it takes room for.
        entry.remove().tell(Message::Error(ErrorBody {
            code: code!("machine.no_answer"),
            retryable: true,
        }));
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u32, Waiter>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How many other asks wait for answers on the same way back as the one under `id`.
fn sharing(waiting: &HashMap<u32, Waiter>, id: u32) -> usize {
    let Some(asker) = waiting.get(&id) else {
        return 0;
    };
    let same = waiting
        .values()
        .filter(|waiter| waiter.back.same_channel(&asker.back));
    same.count().saturating_sub(1)
}

/// Refuses unless `machine` is the asker's own.
///
/// # Errors
/// `machine.not_found`, or the database's failure.
pub(crate) async fn theirs(
    shared: &Shared,
    ask: &Ask<'_>,
    machine: &MachineId,
) -> Result<(), Refusal> {
    let theirs = shared.db.has_machine(&ask.asker.account, machine).await;
    if theirs.map_err(Refusal::Failed)? {
        Ok(())
    } else {
        Err(refused(code!("machine.not_found"), false))
    }
}

pub(crate) const fn refused(code: Code, retryable: bool) -> Refusal {
    Refusal::Refused(ErrorBody { code, retryable })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::task::yield_now;
    use tokio::time::timeout;

    use weacon_protocol::{
        AccessLevel, AccountId, AgentKind, AgentSession, ChatId, DiffContent, DiffFile, DiffLine,
        DiffQuery, DiffQueryKind, ErrorBody, FileChange, Hunk, LineOp, MAX_FRAME, MachineSettings,
        MachineSettingsAction, MachineSettingsAnswer, MachineSettingsAnswerKind,
        MachineSettingsCommand, MachineSettingsQuery, MachineSettingsQueryKind, MemberId,
        OutsideSession, RequestId, SessionsAnswer, SessionsAnswerKind, SessionsQuery,
        SessionsQueryKind, Symbol, SymbolKind, SymbolsAnswer, SymbolsAnswerKind, SymbolsQuery,
        SymbolsQueryKind, WorktreeAction, WorktreeAnswer, WorktreeAnswerKind, WorktreeCommand,
        WorktreeQuery, WorktreeQueryKind,
    };

    use super::*;
    use crate::db::test_support::{Making, make_as};
    use crate::links::{Attached, Connected, QUEUE};
    use crate::test_support::socket::{Socket, ends, next, next_envelope, open, pongs, say, send};
    use crate::test_support::{
        Fixture, PATIENCE, Paired, ask, member_id, pair_machine, workspace_id,
    };

    // A query for `machine`'s settings in `workspace`, claiming to be from someone else.
    fn query(machine: &MachineId, in_workspace: &str) -> Message {
        Message::MachineSettingsQuery(MachineSettingsQuery {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            machine: machine.clone(),
            workspace: workspace_id(in_workspace),
            query: MachineSettingsQueryKind::Current,
            by: Some(Asker {
                account: AccountId::try_from("a-forged".to_owned()).unwrap(),
                member: MemberId::try_from("m-forged".to_owned()).unwrap(),
            }),
        })
    }

    // A command to `machine` in w1, claiming to be from someone else.
    fn command(machine: &MachineId) -> Message {
        let Message::MachineSettingsQuery(query) = query(machine, "w1") else {
            panic!();
        };
        Message::MachineSettingsCommand(MachineSettingsCommand {
            request: query.request,
            machine: query.machine,
            workspace: query.workspace,
            action: MachineSettingsAction::SetReports { send: true },
            by: query.by,
        })
    }

    fn settings_answer(machine: &MachineId) -> Message {
        changed(machine, |_| {})
    }

    // `machine`'s answer, changed by `change`.
    fn changed(machine: &MachineId, change: impl Fn(&mut MachineSettingsAnswer)) -> Message {
        let mut answer = MachineSettingsAnswer {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            machine: machine.clone(),
            workspace: workspace_id("w1"),
            answer: MachineSettingsAnswerKind::Current(MachineSettings {
                reports: None,
                access: AccessLevel::AskFirst,
                team: false,
                shared: false,
                sign_ins: Vec::new(),
            }),
        };
        change(&mut answer);
        Message::MachineSettingsAnswer(answer)
    }

    // Once every one of `timers` has stopped.
    async fn stopped(timers: &[AbortHandle]) {
        while !timers.iter().all(AbortHandle::is_finished) {
            yield_now().await;
        }
    }

    // The next envelope on `channel`, which comes well within a test's time.
    async fn heard(channel: &mut mpsc::Receiver<Envelope>) -> Envelope {
        timeout(PATIENCE, channel.recv()).await.unwrap().unwrap()
    }

    // `paired`'s core, connected and offering `capabilities`, once its connection is relayed to.
    async fn connected(address: SocketAddr, paired: &Paired, capabilities: &[&str]) -> Socket {
        let mut core = open(address, "/api/machines/connect", &paired.token)
            .await
            .unwrap();
        say(&mut core, capabilities).await;
        assert!(matches!(next(&mut core).await, Message::Welcome(_)));
        assert!(pongs(&mut core).await);
        core
    }

    // Ada's machine, connected as its core, and her feed of w1 offering `capabilities`.
    async fn both(
        fixture: &Fixture,
        address: SocketAddr,
        capabilities: &[&str],
    ) -> (Paired, Socket, Socket) {
        let paired = pair_machine(fixture, address).await;
        let core = connected(address, &paired, &["machine.connect", "machine.settings"]).await;
        let mut browser = open(address, "/api/workspaces/w1/feed", &paired.session)
            .await
            .unwrap();
        say(&mut browser, capabilities).await;
        assert!(matches!(next(&mut browser).await, Message::Welcome(_)));
        // Ada joined w1 when she opened it.
        assert!(matches!(next(&mut browser).await, Message::Workspace(_)));
        (paired, core, browser)
    }

    const fn envelope(id: u32, message: Message) -> Envelope {
        Envelope { id, message }
    }

    // What the feed answers `message`, sent under `id`.
    async fn asked(browser: &mut Socket, id: u32, message: Message) -> Envelope {
        send(browser, envelope(id, message)).await;
        next_envelope(browser).await
    }

    fn error_of(envelope: &Envelope) -> (u32, &str, bool) {
        let Message::Error(ErrorBody { code, retryable }) = &envelope.message else {
            panic!("{envelope:?}");
        };
        (envelope.id, code.as_str(), *retryable)
    }

    // Grace, another member of w1 with no machine, and her feed of it offering `capabilities`.
    async fn graces_feed(
        fixture: &Fixture,
        address: SocketAddr,
        capabilities: &[&str],
    ) -> (AccountId, Socket) {
        let db = &fixture.shared.db;
        let email = crate::test_support::email("grace@example.com");
        let account = crate::test_support::fresh_account();
        db.add_account(&account, &email).await.unwrap();
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();
        w1.add_member(&member_id("m5"), &account, &email)
            .await
            .unwrap();
        let grace = fixture.sign_in_client(address, "grace@example.com").await;
        let mut browser = open(address, "/api/workspaces/w1/feed", &grace.token)
            .await
            .unwrap();
        say(&mut browser, capabilities).await;
        assert!(matches!(next(&mut browser).await, Message::Welcome(_)));
        (account, browser)
    }

    // What the feed answers `message`, sent under `id`, past what it says of the workspace.
    async fn answer_to(browser: &mut Socket, id: u32, message: Message) -> Envelope {
        send(browser, envelope(id, message)).await;
        let mut heard = next_envelope(browser).await;
        while heard.id != id {
            heard = next_envelope(browser).await;
        }
        heard
    }

    // A query of `kind` for `machine`'s settings in w1.
    fn of_kind(machine: &MachineId, kind: MachineSettingsQueryKind) -> Message {
        let Message::MachineSettingsQuery(mut asked) = query(machine, "w1") else {
            panic!();
        };
        asked.query = kind;
        Message::MachineSettingsQuery(asked)
    }

    // Who a relayed settings query says asked, and what.
    fn asked_by(relayed: Envelope) -> Option<(AccountId, MachineSettingsQueryKind)> {
        let Message::MachineSettingsQuery(asked) = relayed.message else {
            return None;
        };
        Some((asked.by?.account, asked.query))
    }

    #[tokio::test]
    async fn a_member_asks_a_machine_shared_with_their_workspace_for_its_repos_and_agents_alone() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let offers = ["workspace.feed", "machine.settings"];
        let (paired, mut core, _adas) = both(&fixture, server.address, &offers).await;
        let machine = paired.machine_id();
        let (account, mut browser) = graces_feed(&fixture, server.address, &offers).await;
        let repos = || of_kind(&machine, MachineSettingsQueryKind::Repos);
        let agents = of_kind(&machine, MachineSettingsQueryKind::Agents);

        let unshared = answer_to(&mut browser, 40, repos()).await;
        let db = &fixture.shared.db;
        db.set_shares(&machine, &[workspace_id("w1")])
            .await
            .unwrap();
        send(&mut browser, envelope(41, repos())).await;
        let relayed = next_envelope(&mut core).await;
        let all = answer_to(&mut browser, 42, query(&machine, "w1")).await;
        let change = answer_to(&mut browser, 43, command(&machine)).await;
        send(&mut browser, envelope(44, agents)).await;
        let asked_agents = next_envelope(&mut core).await;

        assert_eq!(error_of(&unshared), (40, "machine.not_found", false));
        // Stamped as hers, whatever her browser claimed.
        let hers = |kind| Some((account.clone(), kind));
        assert_eq!(asked_by(relayed), hers(MachineSettingsQueryKind::Repos));
        assert_eq!(
            asked_by(asked_agents),
            hers(MachineSettingsQueryKind::Agents)
        );
        assert_eq!(error_of(&all), (42, "machine.not_found", false));
        assert_eq!(error_of(&change), (43, "machine.not_found", false));
    }

    #[tokio::test]
    async fn a_query_reaches_the_owners_machine_as_theirs_and_its_answer_comes_back() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, mut core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        let machine = paired.machine_id();
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;

        send(&mut browser, envelope(41, query(&machine, "w1"))).await;
        let relayed = next_envelope(&mut core).await;
        send(&mut core, envelope(relayed.id, settings_answer(&machine))).await;
        let answered = next_envelope(&mut browser).await;
        send(&mut browser, envelope(42, command(&machine))).await;
        let refused = Message::Error(ErrorBody {
            code: code!("settings.not_owner"),
            retryable: false,
        });
        let commanded = next_envelope(&mut core).await;
        send(&mut core, envelope(commanded.id, refused)).await;
        let refusal_back = next_envelope(&mut browser).await;

        let Message::MachineSettingsQuery(query) = relayed.message else {
            panic!("{relayed:?}");
        };
        let Message::MachineSettingsCommand(command) = commanded.message else {
            panic!("{commanded:?}");
        };
        for by in [query.by.unwrap(), command.by.unwrap()] {
            assert_eq!(by.account.as_str(), session["account"].as_str().unwrap());
            assert_eq!(by.member.as_str(), "m1");
        }
        assert_ne!(relayed.id, 41);
        assert_eq!(answered, envelope(41, settings_answer(&machine)));
        assert_eq!(error_of(&refusal_back), (42, "settings.not_owner", false));
    }

    #[tokio::test]
    async fn a_query_for_a_machine_not_theirs_another_workspace_or_one_offline_is_refused() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        let machine = paired.machine_id();
        let stranger = MachineId::try_from("mc-stranger".to_owned()).unwrap();

        let not_theirs = asked(&mut browser, 1, query(&stranger, "w1")).await;
        let elsewhere = asked(&mut browser, 2, query(&machine, "w9")).await;
        // Ada's other machines: one whose core isn't connected, one that doesn't take settings.
        let unconnected = pair_machine(&fixture, server.address).await.machine_id();
        let offline = asked(&mut browser, 4, query(&unconnected, "w1")).await;
        let older = pair_machine(&fixture, server.address).await;
        let _older_core = connected(server.address, &older, &["machine.connect"]).await;
        let unsettled = asked(&mut browser, 5, query(&older.machine_id(), "w1")).await;
        drop(core);

        assert_eq!(error_of(&not_theirs), (1, "machine.not_found", false));
        assert_eq!(error_of(&elsewhere), (2, "machine.not_found", false));
        assert_eq!(error_of(&offline), (4, "machine.offline", true));
        assert_eq!(error_of(&unsettled), (5, "machine.offline", true));
    }

    #[tokio::test]
    async fn a_feed_whose_member_is_one_no_more_relays_nothing_and_ends() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        w1.leave(&member_id("m1")).await.unwrap();

        send(&mut browser, envelope(1, query(&paired.machine_id(), "w1"))).await;

        assert!(ends(&mut browser).await);
        assert!(fixture.shared.relay.lock().is_empty());
        drop(core);
    }

    #[tokio::test]
    async fn a_member_who_leaves_while_their_machine_answers_hears_nothing_more() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, mut core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        let machine = paired.machine_id();

        send(&mut browser, envelope(1, query(&machine, "w1"))).await;
        let relayed = next_envelope(&mut core).await;
        let w1 = fixture.shared.db.workspace(&workspace_id("w1")).await;
        w1.unwrap().leave(&member_id("m1")).await.unwrap();
        send(&mut core, envelope(relayed.id, settings_answer(&machine))).await;

        assert!(ends(&mut browser).await);
    }

    #[tokio::test]
    async fn a_database_that_fails_while_relaying_ends_the_feed() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, _core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        fixture.shared.db.close().await;

        send(&mut browser, envelope(1, query(&paired.machine_id(), "w1"))).await;

        assert!(ends(&mut browser).await);
        assert!(
            fixture
                .records
                .lock()
                .unwrap()
                .iter()
                .any(|record| record.msg == "feed failed")
        );
    }

    #[tokio::test]
    async fn a_client_that_doesnt_offer_the_settings_relays_nothing() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, core, mut browser) = both(&fixture, server.address, &["workspace.feed"]).await;
        let machine = paired.machine_id();

        send(&mut browser, envelope(1, query(&machine, "w1"))).await;
        send(&mut browser, envelope(2, settings_answer(&machine))).await;
        assert!(pongs(&mut browser).await);

        assert!(fixture.shared.relay.lock().is_empty());
        drop(core);
    }

    // A list of the sessions on `machine`, claiming to be from someone else.
    fn sessions_query(machine: &MachineId) -> Message {
        Message::SessionsQuery(SessionsQuery {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            machine: machine.clone(),
            query: SessionsQueryKind::List,
            by: Some(AccountId::try_from("a-forged".to_owned()).unwrap()),
        })
    }

    // `machine`'s list of one session, run in `folder`.
    fn sessions_answer(machine: &MachineId, folder: &str) -> Message {
        sessions_answer_to(machine, "r-1", folder)
    }

    fn sessions_answer_to(machine: &MachineId, request: &str, folder: &str) -> Message {
        let session = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d".to_owned();
        Message::SessionsAnswer(SessionsAnswer {
            request: RequestId::try_from(request.to_owned()).unwrap(),
            machine: machine.clone(),
            answer: SessionsAnswerKind::List {
                sessions: vec![OutsideSession {
                    agent: AgentKind::Claude,
                    session: AgentSession::try_from(session).unwrap(),
                    folder: Some(folder.to_owned()),
                    modified_at: 0,
                }],
            },
        })
    }

    // Ada's machine, its core offering `core_offers`, and her feed of w1 offering sessions.
    async fn sessions_both(
        fixture: &Fixture,
        address: SocketAddr,
        core_offers: &[&str],
    ) -> (Paired, Socket, Socket) {
        let paired = pair_machine(fixture, address).await;
        let core = connected(address, &paired, core_offers).await;
        let mut browser = open(address, "/api/workspaces/w1/feed", &paired.session)
            .await
            .unwrap();
        say(&mut browser, &["workspace.feed", "sessions.read"]).await;
        assert!(matches!(next(&mut browser).await, Message::Welcome(_)));
        assert!(matches!(next(&mut browser).await, Message::Workspace(_)));
        (paired, core, browser)
    }

    #[tokio::test]
    async fn a_persons_own_sessions_reach_them_as_theirs_and_nothing_of_them_is_logged() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let offers = ["machine.connect", "sessions.read"];
        let (paired, mut core, mut browser) =
            sessions_both(&fixture, server.address, &offers).await;
        let machine = paired.machine_id();
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;

        send(&mut browser, envelope(51, sessions_query(&machine))).await;
        let relayed = next_envelope(&mut core).await;
        let answer = sessions_answer(&machine, "/MARKER-FOLDER");
        send(&mut core, envelope(relayed.id, answer.clone())).await;
        let answered = next_envelope(&mut browser).await;

        let Message::SessionsQuery(query) = relayed.message else {
            panic!("{relayed:?}");
        };
        let by = query.by.unwrap();
        assert_eq!(by.as_str(), session["account"].as_str().unwrap());
        assert_eq!(answered, envelope(51, answer));
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_client_that_doesnt_offer_reading_sessions_relays_none() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let offers = &["workspace.feed", "machine.settings"];
        let (paired, mut core, mut browser) = both(&fixture, server.address, offers).await;
        let machine = paired.machine_id();

        send(&mut browser, envelope(51, sessions_query(&machine))).await;
        send(&mut browser, envelope(52, query(&machine, "w1"))).await;

        let relayed = next_envelope(&mut core).await;
        assert!(
            matches!(relayed.message, Message::MachineSettingsQuery(_)),
            "{relayed:?}"
        );
    }

    #[tokio::test]
    async fn sessions_go_only_to_the_askers_own_machine_when_its_core_reads_them() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let offers = ["machine.connect", "machine.settings"];
        let (paired, _core, mut browser) = sessions_both(&fixture, server.address, &offers).await;
        let theirs = MachineId::try_from("mc-elsewhere".to_owned()).unwrap();

        let unread = asked(&mut browser, 52, sessions_query(&paired.machine_id())).await;
        let elsewhere = asked(&mut browser, 53, sessions_query(&theirs)).await;

        assert_eq!(error_of(&unread), (52, "machine.offline", true));
        assert_eq!(error_of(&elsewhere), (53, "machine.not_found", false));
    }

    // A search of `machine`'s code in `workspace`, claiming to be from someone else.
    fn symbols_query(machine: &MachineId, workspace: &str) -> Message {
        let Message::MachineSettingsQuery(settings) = query(machine, workspace) else {
            panic!();
        };
        Message::SymbolsQuery(SymbolsQuery {
            request: settings.request,
            machine: settings.machine,
            workspace: settings.workspace,
            query: SymbolsQueryKind::Search {
                words: "MARKER-WORDS".to_owned(),
            },
            by: settings.by,
        })
    }

    fn symbols_answer(machine: &MachineId, workspace: &str) -> Message {
        Message::SymbolsAnswer(SymbolsAnswer {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            machine: machine.clone(),
            workspace: workspace_id(workspace),
            answer: SymbolsAnswerKind::Found {
                symbols: vec![Symbol {
                    repo: "site".parse().unwrap(),
                    kind: SymbolKind::Fn,
                    name: "MARKER::run".to_owned(),
                    path: "src/MARKER.rs".to_owned(),
                    line: 3,
                }],
                more: false,
            },
        })
    }

    #[tokio::test]
    async fn a_search_of_the_askers_own_code_reaches_their_machine_and_nothing_of_it_is_logged() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let offers = ["machine.connect", "symbols.search"];
        let mut core = connected(server.address, &paired, &offers).await;
        let mut browser = open(server.address, "/api/workspaces/w1/feed", &paired.session)
            .await
            .unwrap();
        say(&mut browser, &["workspace.feed", "symbols.search"]).await;
        assert!(matches!(next(&mut browser).await, Message::Welcome(_)));
        assert!(matches!(next(&mut browser).await, Message::Workspace(_)));
        let machine = paired.machine_id();

        let elsewhere = asked(&mut browser, 50, symbols_query(&machine, "w2")).await;
        send(&mut browser, envelope(51, symbols_query(&machine, "w1"))).await;
        let relayed = next_envelope(&mut core).await;
        let other_workspace = symbols_answer(&machine, "w2");
        send(&mut core, envelope(relayed.id, other_workspace)).await;
        let answer = symbols_answer(&machine, "w1");
        send(&mut core, envelope(relayed.id, answer.clone())).await;
        let answered = next_envelope(&mut browser).await;

        assert_eq!(error_of(&elsewhere), (50, "machine.not_found", false));
        let Message::SymbolsQuery(query) = relayed.message else {
            panic!("{relayed:?}");
        };
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let by = query.by.unwrap();
        assert_eq!(by.account.as_str(), session["account"].as_str().unwrap());
        assert_eq!(by.member.as_str(), "m1");
        assert_eq!(answered, envelope(51, answer));
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[test]
    fn a_search_needs_symbols_search_and_its_answer_is_taken_back() {
        let machine = MachineId::try_from("mc-1".to_owned()).unwrap();
        assert_eq!(needs(&symbols_query(&machine, "w1")), Some(SYMBOLS));
        assert_eq!(needs(&symbols_answer(&machine, "w1")), None);
    }

    // A relay where `machine`'s own sessions were asked under 41, relayed as 7.
    fn sessions_waiting(machine: &MachineId) -> (Arc<Relay>, mpsc::Receiver<Envelope>) {
        let relay = Arc::new(Relay::new(ANSWER_WITHIN));
        let (back, answers) = mpsc::channel(4);
        let waiter = Waiter {
            sent: Sent {
                asked: Asked {
                    workspace: None,
                    request: RequestId::try_from("r-1".to_owned()).unwrap(),
                    about: About::Sessions(machine.clone()),
                },
                machine: machine.clone(),
            },
            id: 41,
            back,
            files_left: None,
            timer: tokio::spawn(async {}).abort_handle(),
            round: 0,
        };
        relay.lock().insert(7, waiter);
        (relay, answers)
    }

    #[tokio::test]
    async fn a_sessions_ask_takes_only_its_own_machines_sessions_answer() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let two = MachineId::try_from("mc-2".to_owned()).unwrap();
        let (relay, mut answers) = sessions_waiting(&one);

        let dropped = [
            relay.answered(&one, envelope(7, settings_answer(&one))),
            relay.answered(&two, envelope(7, sessions_answer(&one, "/work"))),
            relay.answered(&one, envelope(7, sessions_answer(&two, "/work"))),
            relay.answered(&one, envelope(7, sessions_answer_to(&one, "r-2", "/work"))),
        ];
        let before = answers.try_recv();
        let answered = relay.answered(&one, envelope(7, sessions_answer(&one, "/work")));

        assert_eq!(dropped, [true; 4]);
        before.unwrap_err();
        assert!(answered);
        assert_eq!(
            heard(&mut answers).await,
            envelope(41, sessions_answer(&one, "/work"))
        );
    }

    // A relay where `machine` was asked under 41, relayed as 7; and where its answer goes.
    fn waiting_on(machine: &MachineId) -> (Arc<Relay>, mpsc::Receiver<Envelope>) {
        let relay = Arc::new(Relay::new(ANSWER_WITHIN));
        let (back, answers) = mpsc::channel(4);
        let waiter = Waiter {
            sent: Sent {
                asked: Asked {
                    workspace: Some(workspace_id("w1")),
                    request: RequestId::try_from("r-1".to_owned()).unwrap(),
                    about: About::Machine(machine.clone()),
                },
                machine: machine.clone(),
            },
            id: 41,
            back,
            files_left: None,
            timer: tokio::spawn(async {}).abort_handle(),
            round: 0,
        };
        relay.lock().insert(7, waiter);
        (relay, answers)
    }

    fn accepted(request: &str) -> Message {
        Message::Receipt(weacon_protocol::Receipt {
            request: RequestId::try_from(request.to_owned()).unwrap(),
            outcome: weacon_protocol::Outcome::Accepted,
        })
    }

    #[tokio::test]
    async fn a_chat_commands_receipt_answers_it_only_from_its_machine_and_for_its_request() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let two = MachineId::try_from("mc-2".to_owned()).unwrap();
        let (relay, mut answers) = waiting_on(&one);
        if let Some(waiter) = relay.lock().get_mut(&7) {
            let chat = ChatId::try_from("c-1".to_owned()).unwrap();
            waiter.sent.asked.about = About::Command(chat);
        }

        let claimed = [
            relay.answered(&two, envelope(7, accepted("r-1"))),
            relay.answered(&one, envelope(7, accepted("r-2"))),
        ];
        let before = answers.try_recv();
        let answered = relay.answered(&one, envelope(7, accepted("r-1")));

        assert_eq!(claimed, [true, true]);
        before.unwrap_err();
        assert!(answered);
        let told = answers.try_recv().unwrap();
        assert_eq!((told.id, told.message), (41, accepted("r-1")));
        assert!(!relay.answered(&one, envelope(7, accepted("r-1"))));
    }

    #[tokio::test]
    async fn only_the_asked_machine_answers_what_was_asked_and_only_once() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let two = MachineId::try_from("mc-2".to_owned()).unwrap();
        let (relay, mut answers) = waiting_on(&one);

        let elsewhere = |answer: &mut MachineSettingsAnswer| answer.workspace = workspace_id("w2");
        let another = |answer: &mut MachineSettingsAnswer| {
            answer.request = RequestId::try_from("r-2".to_owned()).unwrap();
        };
        let refused = Message::Error(ErrorBody {
            code: code!("settings.not_owner"),
            retryable: false,
        });

        let claimed = [
            relay.answered(&two, envelope(7, settings_answer(&one))),
            relay.answered(&two, envelope(7, settings_answer(&two))),
            relay.answered(&two, envelope(7, refused)),
            relay.answered(&one, envelope(7, settings_answer(&two))),
            relay.answered(&one, envelope(7, changed(&one, elsewhere))),
            relay.answered(&one, envelope(7, changed(&one, another))),
            relay.answered(&one, envelope(8, settings_answer(&one))),
            relay.answered(&one, envelope(7, Message::Unknown)),
            // A receipt answers only a chat command.
            relay.answered(&one, envelope(7, accepted("r-1"))),
        ];
        let before = answers.try_recv();
        let answered = relay.answered(&one, envelope(7, settings_answer(&one)));
        let again = relay.answered(&one, envelope(7, settings_answer(&one)));

        assert_eq!(
            claimed,
            [true, true, true, true, true, true, false, true, true]
        );
        assert!(answered);
        assert!(!again);
        before.unwrap_err();
        assert_eq!(
            heard(&mut answers).await,
            envelope(41, settings_answer(&one))
        );
        answers.try_recv().unwrap_err();
    }

    #[tokio::test]
    async fn an_answer_this_server_cant_send_on_is_refused_to_the_asker() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let (relay, mut answers) = waiting_on(&one);
        let newer = changed(&one, |answer| {
            answer.answer = MachineSettingsAnswerKind::Newer;
        });

        assert!(relay.answered(&one, envelope(7, newer)));

        let refused = heard(&mut answers).await;
        assert_eq!(error_of(&refused), (41, "relay.unsendable", false));
    }

    #[tokio::test]
    async fn an_error_from_the_asked_machine_comes_back() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let (relay, mut answers) = waiting_on(&one);
        let refused = Message::Error(ErrorBody {
            code: code!("settings.not_owner"),
            retryable: false,
        });

        assert!(relay.answered(&one, envelope(7, refused.clone())));

        assert_eq!(heard(&mut answers).await, envelope(41, refused));
    }

    // Ada's paired machine, connected and offering settings, and her as an asker in w1.
    async fn adas(fixture: &Fixture) -> (MachineId, Asker, Attached) {
        adas_offering(fixture, &[SETTINGS]).await
    }

    // Ada's paired machine, connected and offering `offers`, and her as an asker in w1.
    async fn adas_offering(fixture: &Fixture, offers: &[Code]) -> (MachineId, Asker, Attached) {
        let server = fixture.run().await;
        let paired = pair_machine(fixture, server.address).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();
        let machine = paired.machine_id();
        let asker = Asker {
            account,
            member: member_id("m1"),
        };
        let attached = relink(fixture, &machine, &asker, offers);
        (machine, asker, attached)
    }

    // `machine`, of `asker`, connected afresh and offering `offers`.
    fn relink(fixture: &Fixture, machine: &MachineId, asker: &Asker, offers: &[Code]) -> Attached {
        let connected = Connected {
            machine: machine.clone(),
            owner: asker.account.clone(),
        };
        let offers = offers.iter().cloned().map(Capability::new).collect();
        fixture.shared.links.open(connected, offers)
    }

    fn code_of(result: Result<(), Refusal>) -> Option<String> {
        match result {
            Ok(()) => None,
            Err(Refusal::Refused(body)) => Some(body.code.as_str().to_owned()),
            Err(Refusal::Failed(fault)) => Some(format!("failed {}", fault.code().as_str())),
        }
    }

    #[tokio::test]
    async fn each_asker_has_room_for_its_answers() {
        let fixture = Fixture::new().await;
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        let ((one, _one_answers), (two, _two_answers)) = (mpsc::channel(1), mpsc::channel(4));
        let to = |id| envelope(id, query(&machine, "w1"));

        let mut codes = Vec::new();
        for (id, back) in [(1, &one), (2, &one), (3, &two), (4, &two)] {
            codes.push(code_of(forward(&fixture.shared, &ask, to(id), back).await));
        }

        let busy = Some("relay.busy".to_owned());
        assert_eq!(codes, [None, busy, None, None]);
        assert_eq!(fixture.shared.relay.lock().len(), 3);
    }

    #[tokio::test]
    async fn an_ask_unanswered_in_time_is_told_so() {
        let mut fixture = Fixture::new().await;
        fixture.shared.relay = Arc::new(Relay::new(Duration::ZERO));
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        let (back, mut answers) = mpsc::channel(4);

        let sent = forward(
            &fixture.shared,
            &ask,
            envelope(1, query(&machine, "w1")),
            &back,
        )
        .await;
        let told = heard(&mut answers).await;

        assert_eq!(code_of(sent), None);
        assert_eq!(error_of(&told), (1, "machine.no_answer", true));
        assert!(fixture.shared.relay.lock().is_empty());
    }

    #[tokio::test]
    async fn a_machine_too_slow_to_read_is_offline_and_its_ask_doesnt_wait() {
        let fixture = Fixture::new().await;
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        let (back, _answers) = mpsc::channel(QUEUE + 1);

        let mut codes = Vec::new();
        for id in 0..=u32::try_from(QUEUE).unwrap() {
            let sent = envelope(id, query(&machine, "w1"));
            codes.push(code_of(forward(&fixture.shared, &ask, sent, &back).await));
        }

        assert!(codes.iter().take(QUEUE).all(Option::is_none));
        assert_eq!(codes.last(), Some(&Some("machine.offline".to_owned())));
        assert_eq!(fixture.shared.relay.lock().len(), QUEUE);
    }

    #[tokio::test]
    async fn askers_with_the_same_id_each_get_their_own_answer() {
        let fixture = Fixture::new().await;
        let (machine, asker, mut attached) = adas(&fixture).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        let ((one, mut to_one), (two, mut to_two)) = (mpsc::channel(4), mpsc::channel(4));
        let failed = |code| {
            Message::Error(ErrorBody {
                code,
                retryable: false,
            })
        };

        for back in [&one, &two] {
            let sent = envelope(1, query(&machine, "w1"));
            assert_eq!(
                code_of(forward(&fixture.shared, &ask, sent, back).await),
                None
            );
        }
        let timers: Vec<_> = (fixture.shared.relay.lock().values())
            .map(|waiter| waiter.timer.clone())
            .collect();
        let first = heard(&mut attached.incoming).await.id;
        let second = heard(&mut attached.incoming).await.id;
        let relay = &fixture.shared.relay;
        assert!(relay.answered(&machine, envelope(second, failed(code!("test.second")))));
        assert!(relay.answered(&machine, envelope(first, failed(code!("test.first")))));

        assert_eq!(
            error_of(&heard(&mut to_one).await),
            (1, "test.first", false)
        );
        assert_eq!(
            error_of(&heard(&mut to_two).await),
            (1, "test.second", false)
        );
        to_one.try_recv().unwrap_err();
        to_two.try_recv().unwrap_err();
        // An answered ask's timer stops with it.
        timeout(PATIENCE, stopped(&timers)).await.unwrap();
        assert_eq!(timers.len(), 2);
    }

    #[tokio::test]
    async fn only_queries_and_commands_are_carried() {
        let fixture = Fixture::new().await;
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };

        let sent = envelope(1, settings_answer(&machine));
        let result = forward(&fixture.shared, &ask, sent, &mpsc::channel(4).0).await;
        let Message::MachineSettingsQuery(mut newer) = query(&machine, "w1") else {
            panic!();
        };
        newer.query = MachineSettingsQueryKind::Newer;
        let sent = envelope(2, Message::MachineSettingsQuery(newer));
        let unsendable = forward(&fixture.shared, &ask, sent, &mpsc::channel(4).0).await;

        assert_eq!(code_of(unsendable), Some("relay.unknown".to_owned()));
        assert!(fixture.shared.relay.lock().is_empty());
        assert_eq!(needs(&query(&machine, "w1")), Some(SETTINGS));
        assert_eq!(needs(&command(&machine)), Some(SETTINGS));
        assert_eq!(needs(&sessions_query(&machine)), Some(SESSIONS));
        assert_eq!(needs(&settings_answer(&machine)), None);
        assert_eq!(code_of(result), Some("relay.unknown".to_owned()));
    }

    #[tokio::test]
    async fn a_database_that_fails_fails_the_relay() {
        let fixture = Fixture::new().await;
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        fixture.shared.db.close().await;

        let sent = envelope(1, query(&machine, "w1"));
        let result = forward(&fixture.shared, &ask, sent, &mpsc::channel(4).0).await;

        assert_eq!(code_of(result), Some("failed db.failed".to_owned()));
        assert!(fixture.shared.relay.lock().is_empty());
    }

    // A chat's diff and worktree asks, which the server sends to the chat's machine.

    fn forged() -> Asker {
        Asker {
            account: AccountId::try_from("a-forged".to_owned()).unwrap(),
            member: MemberId::try_from("m-forged".to_owned()).unwrap(),
        }
    }

    fn request() -> RequestId {
        RequestId::try_from("r-1".to_owned()).unwrap()
    }

    fn chat_id(chat: &str) -> ChatId {
        ChatId::try_from(chat.to_owned()).unwrap()
    }

    fn diff_query(chat: &str, in_workspace: &str) -> Message {
        Message::DiffQuery(DiffQuery {
            machine: None,
            request: request(),
            workspace: workspace_id(in_workspace),
            chat: chat_id(chat),
            query: DiffQueryKind::Changes,
            by: Some(forged()),
        })
    }

    fn worktree_query(chat: &str) -> Message {
        Message::WorktreeQuery(WorktreeQuery {
            machine: None,
            request: request(),
            workspace: workspace_id("w1"),
            chat: chat_id(chat),
            query: WorktreeQueryKind::State,
            by: Some(forged()),
        })
    }

    // `message`, about a chat's agent, naming the machine it is on.
    fn on(machine: &MachineId, mut message: Message) -> Message {
        let named = if let Message::DiffQuery(query) = &mut message {
            &mut query.machine
        } else if let Message::WorktreeQuery(query) = &mut message {
            &mut query.machine
        } else if let Message::WorktreeCommand(command) = &mut message {
            &mut command.machine
        } else {
            panic!("not about a chat's agent: {message:?}");
        };
        *named = Some(machine.clone());
        message
    }

    fn worktree_command(chat: &str, action: WorktreeAction) -> Message {
        Message::WorktreeCommand(WorktreeCommand {
            machine: None,
            request: request(),
            workspace: workspace_id("w1"),
            chat: chat_id(chat),
            action,
            by: Some(forged()),
        })
    }

    // One of a diff's `files` files, whose one line is `text`.
    fn file(files: u32, path: &str, text: &str) -> Message {
        let line = DiffLine {
            op: LineOp::Added,
            text: text.to_owned(),
            no_newline: false,
        };
        let hunk = Hunk {
            old_start: 0,
            new_start: 1,
            lines: vec![line],
        };
        Message::DiffAnswer(DiffAnswer {
            request: request(),
            workspace: workspace_id("w1"),
            chat: chat_id("c-1"),
            answer: DiffAnswerKind::File {
                files,
                file: DiffFile {
                    path: path.to_owned(),
                    change: FileChange::Added,
                    content: DiffContent::Text { hunks: vec![hunk] },
                },
            },
        })
    }

    fn state() -> Message {
        Message::WorktreeAnswer(WorktreeAnswer {
            request: request(),
            workspace: workspace_id("w1"),
            chat: chat_id("c-1"),
            answer: WorktreeAnswerKind::State {
                removed: false,
                uncommitted: true,
                unpushed: false,
            },
        })
    }

    // m1's chat `chat` in w1, on `machine`.
    async fn chat_on(fixture: &Fixture, chat: &str, machine: &MachineId) {
        let w1 = fixture.shared.db.workspace(&workspace_id("w1")).await;
        let making = Making {
            chat,
            owner: "m1",
            title: "Fix the build",
            machine: machine.as_str(),
            at: 1,
            task: None,
        };
        make_as(&w1.unwrap(), &making).await.unwrap();
    }

    // Ada's machine, connected as a core offering `offers`, her chat c-1 on it, and her
    // feed of w1 offering diffs and worktrees.
    async fn chat_and_feed(
        fixture: &Fixture,
        address: SocketAddr,
        offers: &[&str],
    ) -> (Paired, Socket, Socket) {
        let paired = pair_machine(fixture, address).await;
        chat_on(fixture, "c-1", &paired.machine_id()).await;
        let core = connected(address, &paired, offers).await;
        let mut browser = open(address, "/api/workspaces/w1/feed", &paired.session)
            .await
            .unwrap();
        say(
            &mut browser,
            &["workspace.feed", "diff.view", "worktree.manage"],
        )
        .await;
        assert!(matches!(next(&mut browser).await, Message::Welcome(_)));
        assert!(matches!(next(&mut browser).await, Message::Workspace(_)));
        (paired, core, browser)
    }

    const ALL: [&str; 4] = [
        "machine.connect",
        "diff.view",
        "worktree.manage",
        "worktree.open_terminal",
    ];

    #[tokio::test]
    async fn a_member_reads_the_changes_of_an_agent_on_a_shared_machine_and_changes_nothing() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine_id();
        let mut core = connected(server.address, &paired, &ALL).await;
        let reads = ["workspace.feed", "diff.view", "worktree.manage"];
        let (account, mut grace) = graces_feed(&fixture, server.address, &reads).await;
        let mut ada = open(server.address, "/api/workspaces/w1/feed", &paired.session)
            .await
            .unwrap();
        say(&mut ada, &reads).await;
        let db = &fixture.shared.db;
        db.set_shares(&machine, &[workspace_id("w1")])
            .await
            .unwrap();
        chat_on(&fixture, "c-1", &machine).await;

        let diff_of = || on(&machine, diff_query("c-1", "w1"));
        send(&mut grace, envelope(41, diff_of())).await;
        let diff = next_envelope(&mut core).await;
        let state_of = on(&machine, worktree_query("c-1"));
        send(&mut grace, envelope(42, state_of)).await;
        let state = next_envelope(&mut core).await;
        let remove = on(&machine, worktree_command("c-1", WorktreeAction::Remove));
        let changed = answer_to(&mut grace, 43, remove).await;
        send(&mut ada, envelope(45, diff_of())).await;
        let owners = next_envelope(&mut core).await;

        let Message::DiffQuery(diff) = diff.message else {
            panic!("{diff:?}");
        };
        let Message::WorktreeQuery(state) = state.message else {
            panic!("{state:?}");
        };
        let asked = [diff.by, state.by].map(|by| by.map(|by| by.account));
        assert_eq!(asked, [Some(account.clone()), Some(account.clone())]);
        // The worktree is its machine's owner's to change; the agent's changes both read.
        assert_eq!(error_of(&changed), (43, "machine.not_found", false));
        let Message::DiffQuery(owners) = owners.message else {
            panic!("{owners:?}");
        };
        assert_ne!(owners.by.unwrap().account, account);
        // Once sharing ends, they are its owner's alone to read.
        db.set_shares(&machine, &[]).await.unwrap();
        let ended = answer_to(&mut grace, 46, diff_of()).await;
        assert_eq!(error_of(&ended), (46, "machine.not_found", false));
    }

    #[tokio::test]
    async fn a_chats_diff_and_worktree_reach_its_machine_as_its_owners_and_come_back() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, mut core, mut browser) = chat_and_feed(&fixture, server.address, &ALL).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let machine = paired.machine_id();

        let diff = on(&machine, diff_query("c-1", "w1"));
        send(&mut browser, envelope(41, diff)).await;
        let relayed = next_envelope(&mut core).await;
        for (path, text) in [("a.rs", "one"), ("b.rs", "two"), ("c.rs", "extra")] {
            send(&mut core, envelope(relayed.id, file(2, path, text))).await;
        }
        let first = next_envelope(&mut browser).await;
        let second = next_envelope(&mut browser).await;
        let remove = on(&machine, worktree_command("c-1", WorktreeAction::Remove));
        send(&mut browser, envelope(42, remove)).await;
        let commanded = next_envelope(&mut core).await;
        send(&mut core, envelope(commanded.id, state())).await;
        let answered = next_envelope(&mut browser).await;

        let Message::DiffQuery(query) = relayed.message else {
            panic!("{relayed:?}");
        };
        let Message::WorktreeCommand(command) = commanded.message else {
            panic!("{commanded:?}");
        };
        for by in [query.by.unwrap(), command.by.unwrap()] {
            assert_eq!(by.account.as_str(), session["account"].as_str().unwrap());
            assert_eq!(by.member.as_str(), "m1");
        }
        assert_eq!(first, envelope(41, file(2, "a.rs", "one")));
        assert_eq!(second, envelope(41, file(2, "b.rs", "two")));
        // The extra file never came: the next answer is the command's.
        assert_eq!(answered, envelope(42, state()));
        assert!(fixture.shared.relay.lock().is_empty());
    }

    #[tokio::test]
    async fn nothing_of_a_diff_or_a_pull_request_is_stored_or_logged() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (paired, mut core, mut browser) = chat_and_feed(&fixture, server.address, &ALL).await;
        let machine = paired.machine_id();

        let diff = on(&machine, diff_query("c-1", "w1"));
        send(&mut browser, envelope(41, diff)).await;
        let relayed = next_envelope(&mut core).await;
        let marked = file(1, "MARKER-PATH.rs", "MARKER-LINE");
        send(&mut core, envelope(relayed.id, marked.clone())).await;
        let diff = next_envelope(&mut browser).await;
        let pull_request = WorktreeAction::OpenPullRequest {
            title: "MARKER-TITLE".to_owned(),
            body: "MARKER-BODY".to_owned(),
        };
        send(
            &mut browser,
            envelope(42, on(&machine, worktree_command("c-1", pull_request))),
        )
        .await;
        let commanded = next_envelope(&mut core).await;
        send(
            &mut core,
            envelope(
                commanded.id,
                Message::Error(ErrorBody {
                    code: code!("worktree.none"),
                    retryable: false,
                }),
            ),
        )
        .await;
        let refused = next_envelope(&mut browser).await;

        assert_eq!(diff, envelope(41, marked));
        assert_eq!(error_of(&refused), (42, "worktree.none", false));
        let logged = format!("{:?}", fixture.records.lock().unwrap());
        let stored = String::from_utf8_lossy(&fixture.stored()).into_owned();
        for marker in ["MARKER-PATH", "MARKER-LINE", "MARKER-TITLE", "MARKER-BODY"] {
            assert!(!logged.contains(marker), "{marker} logged");
            assert!(!stored.contains(marker), "{marker} stored");
        }
    }

    #[tokio::test]
    async fn a_chat_unknown_or_an_agent_on_no_machine_or_one_not_theirs_is_refused() {
        let fixture = Fixture::new().await;
        let (machine, asker, _attached) = adas_offering(&fixture, &[DIFF]).await;
        chat_on(&fixture, "c-1", &machine).await;
        let elsewhere = MachineId::try_from("mc-elsewhere".to_owned()).unwrap();
        chat_on(&fixture, "c-2", &elsewhere).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let back = mpsc::channel(4).0;
        let codes = async |by: &Asker, message| {
            let ask = Ask {
                asker: by,
                workspace: &w1,
            };
            code_of(forward(&fixture.shared, &ask, envelope(1, message), &back).await)
        };

        let refused = [
            codes(&asker, on(&machine, diff_query("c-9", "w1"))).await,
            codes(&asker, on(&machine, diff_query("c-1", "w2"))).await,
            codes(&asker, diff_query("c-1", "w1")).await,
            codes(&asker, on(&machine, diff_query("c-2", "w1"))).await,
            codes(&asker, on(&elsewhere, diff_query("c-2", "w1"))).await,
        ];
        let mine = codes(&asker, on(&machine, diff_query("c-1", "w1"))).await;

        assert_eq!(
            refused.map(Option::unwrap),
            [
                "chat.not_found",
                "chat.not_found",
                // No machine named, and one with no agent in the chat.
                "chat.not_owner",
                "chat.not_owner",
                "machine.not_found"
            ]
        );
        assert_eq!(mine, None);
    }

    #[tokio::test]
    async fn each_ask_goes_only_to_a_machine_that_offers_what_it_needs() {
        let fixture = Fixture::new().await;
        let (machine, asker, attached) = adas_offering(&fixture, &[DIFF, WORKTREE]).await;
        chat_on(&fixture, "c-1", &machine).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        let back = mpsc::channel(8).0;
        let terminal = || {
            on(
                &machine,
                worktree_command("c-1", WorktreeAction::OpenTerminal),
            )
        };
        let sent = async |message| {
            code_of(forward(&fixture.shared, &ask, envelope(1, message), &back).await)
        };

        let offered = [
            sent(on(&machine, diff_query("c-1", "w1"))).await,
            sent(on(&machine, worktree_query("c-1"))).await,
            sent(on(
                &machine,
                worktree_command("c-1", WorktreeAction::Remove),
            ))
            .await,
        ];
        let no_terminal = sent(terminal()).await;
        drop(attached);
        let _with_terminal = relink(&fixture, &machine, &asker, &[TERMINAL]);
        let state_of = on(&machine, worktree_query("c-1"));
        let only_terminal = [sent(terminal()).await, sent(state_of).await];

        assert_eq!(offered, [None, None, None]);
        assert_eq!(no_terminal.as_deref(), Some("machine.offline"));
        assert_eq!(only_terminal, [None, Some("machine.offline".to_owned())]);
        assert_eq!(needs(&terminal()), Some(WORKTREE));
        assert_eq!(needs(&diff_query("c-1", "w1")), Some(DIFF));
        assert_eq!(needs(&state()), None);
    }

    // A relay where a diff of c-1 was asked under 41, relayed as 7; and where its answers
    // go, with room for `room`.
    fn diff_waiting(room: usize, within: Duration) -> (Arc<Relay>, mpsc::Receiver<Envelope>) {
        let relay = Arc::new(Relay::new(within));
        let (back, answers) = mpsc::channel(room);
        let asked = Asked {
            workspace: Some(workspace_id("w1")),
            request: request(),
            about: About::Diff(chat_id("c-1")),
        };
        let sent = Sent {
            asked,
            machine: mc1(),
        };
        relay.wait(7, sent, 41, &back).unwrap();
        (relay, answers)
    }

    fn mc1() -> MachineId {
        MachineId::try_from("mc-1".to_owned()).unwrap()
    }

    #[tokio::test]
    async fn an_asker_that_falls_behind_a_diff_is_told_so_in_its_last_room() {
        let (relay, mut answers) = diff_waiting(2, ANSWER_WITHIN);
        let claimed = [1, 2, 3].map(|_| relay.answered(&mc1(), envelope(7, file(3, "a", "x"))));

        assert_eq!(claimed, [true, true, false]);
        assert_eq!(heard(&mut answers).await, envelope(41, file(3, "a", "x")));
        assert_eq!(
            error_of(&heard(&mut answers).await),
            (41, "relay.busy", true)
        );
        answers.try_recv().unwrap_err();
        assert!(relay.lock().is_empty());
    }

    #[tokio::test]
    async fn a_diff_leaves_room_for_the_askers_other_answers() {
        let (relay, mut answers) = diff_waiting(3, ANSWER_WITHIN);
        let back = relay.lock().get(&7).unwrap().back.clone();
        let asked = Asked {
            workspace: Some(workspace_id("w1")),
            request: request(),
            about: About::Machine(mc1()),
        };
        let sent = Sent {
            asked,
            machine: mc1(),
        };
        relay.wait(8, sent, 42, &back).unwrap();

        let claimed = [1, 2, 3].map(|_| relay.answered(&mc1(), envelope(7, file(9, "a", "x"))));
        assert!(relay.answered(&mc1(), envelope(8, settings_answer(&mc1()))));

        assert_eq!(claimed, [true, true, false]);
        assert_eq!(heard(&mut answers).await, envelope(41, file(9, "a", "x")));
        assert_eq!(
            error_of(&heard(&mut answers).await),
            (41, "relay.busy", true)
        );
        assert_eq!(
            heard(&mut answers).await,
            envelope(42, settings_answer(&mc1()))
        );
        assert!(relay.lock().is_empty());
    }

    #[tokio::test]
    async fn a_timer_a_later_file_restarted_does_nothing() {
        let (relay, mut answers) = diff_waiting(4, ANSWER_WITHIN);
        assert!(relay.answered(&mc1(), envelope(7, file(3, "a", "x"))));
        heard(&mut answers).await;

        relay.expire(7, 0);
        assert!(relay.lock().contains_key(&7));
        relay.expire(7, 1);
        assert_eq!(
            error_of(&heard(&mut answers).await),
            (41, "machine.no_answer", true)
        );
        assert!(relay.lock().is_empty());
    }

    #[tokio::test]
    async fn an_answer_of_another_kind_doesnt_answer_the_ask() {
        let (relay, mut answers) = diff_waiting(4, ANSWER_WITHIN);

        assert!(relay.answered(&mc1(), envelope(7, state())));
        assert!(relay.lock().contains_key(&7));
        answers.try_recv().unwrap_err();
    }

    #[tokio::test]
    async fn a_file_this_server_cant_send_on_ends_the_diff() {
        let (relay, mut answers) = diff_waiting(4, ANSWER_WITHIN);
        let big = "x".repeat(MAX_FRAME);

        assert!(relay.answered(&mc1(), envelope(7, file(3, "a", &big))));
        assert!(!relay.answered(&mc1(), envelope(7, file(3, "b", "x"))));
        assert_eq!(
            error_of(&heard(&mut answers).await),
            (41, "relay.unsendable", false)
        );
        assert!(relay.lock().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn each_of_a_diffs_files_gives_the_next_its_own_time() {
        let within = Duration::from_secs(30);
        let (relay, mut answers) = diff_waiting(4, within);
        for _ in 0..2 {
            sleep(Duration::from_secs(29)).await;
            assert!(relay.answered(&mc1(), envelope(7, file(3, "a", "x"))));
            assert_eq!(heard(&mut answers).await.id, 41);
        }
        sleep(Duration::from_secs(31)).await;

        assert_eq!(
            error_of(&heard(&mut answers).await),
            (41, "machine.no_answer", true)
        );
        assert!(relay.lock().is_empty());
    }
}
