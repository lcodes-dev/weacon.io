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
    Asker, Capability, Code, Envelope, ErrorBody, MachineId, Message, RequestId, WorkspaceId, code,
};

use crate::shared::Shared;

/// How many asks may wait for their answers at once, server-wide.
const MOST_WAITING: usize = 4096;
/// How long an ask waits for its answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// What a machine's core offers, and the server offers, to have its settings relayed.
pub(crate) const SETTINGS: Code = code!("machine.settings");
/// What a machine's core offers, and the server offers, to have a person's own sessions, run
/// outside Weacon, read through it.
pub(crate) const SESSIONS: Code = code!("sessions.read");
/// Everything the relay carries; the server offers each of them on both ends.
pub(crate) const RELAYED: [Code; 2] = [SETTINGS, SESSIONS];

/// The asks waiting for their machines' answers.
pub(crate) struct Relay {
    /// Each ask that waits, under the id it was relayed with.
    waiting: Mutex<HashMap<u32, Waiter>>,
    most_waiting: usize,
    answer_within: Duration,
}

/// An ask's way back to the connection that asked.
struct Waiter {
    /// What it asked, which its answer must name.
    asked: Asked,
    id: u32,
    back: mpsc::Sender<Envelope>,
    /// What tells it, if nothing answers it first, that no answer came.
    timer: AbortHandle,
}

/// The kind, machine, workspace and request an ask or its answer names.
#[derive(PartialEq, Eq)]
struct Asked {
    /// What the kind needs, so an answer of one kind never answers an ask of another.
    kind: Code,
    machine: MachineId,
    /// None for a person's own sessions, which no workspace holds.
    workspace: Option<WorkspaceId>,
    request: RequestId,
}

impl Waiter {
    // The asker that left, or reads too slowly, misses it. An answer from a newer core that
    // this server can't send on is refused, so the asker's connection goes on.
    fn tell(self, message: Message) {
        let mut envelope = Envelope {
            id: self.id,
            message,
        };
        if envelope.to_json().is_err() {
            envelope.message = Message::Error(ErrorBody {
                code: code!("relay.unsendable"),
                retryable: false,
            });
        }
        drop(self.back.try_send(envelope));
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
    pub(crate) workspace: &'a WorkspaceId,
}

/// Why a message wasn't relayed.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The error to answer the asker with.
    Refused(ErrorBody),
    /// The relay failed, as a database can.
    Failed(Fault),
}

/// What both the client and the machine must offer for the relay to forward `message`; `None`
/// for a kind it doesn't forward.
pub(crate) const fn needs(message: &Message) -> Option<Code> {
    if matches!(
        message,
        Message::MachineSettingsQuery(_) | Message::MachineSettingsCommand(_)
    ) {
        Some(SETTINGS)
    } else if matches!(message, Message::SessionsQuery(_)) {
        Some(SESSIONS)
    } else {
        None
    }
}

/// Forwards `envelope` to the machine its message names, if the machine is the asker's own
/// and, for a kind in a workspace, that workspace is the asker's. The answer reaches `back`
/// under the envelope's id, or `machine.no_answer` once it has waited too long.
///
/// # Errors
/// `relay.unknown` for a kind the relay doesn't carry, `machine.not_found` for a machine that
/// isn't the asker's or a workspace that isn't theirs, `machine.offline` and `relay.busy`,
/// which may pass, and the database's failure.
pub(crate) async fn forward(
    shared: &Shared,
    ask: &Ask<'_>,
    envelope: Envelope,
    back: &mpsc::Sender<Envelope>,
) -> Result<(), Refusal> {
    let (asked, message) =
        stamped(envelope.message, ask.asker).ok_or(refused(code!("relay.unknown"), false))?;
    let needs = Capability::from(asked.kind.clone());
    let machine = asked.machine.clone();
    if asked
        .workspace
        .as_ref()
        .is_some_and(|named| named != ask.workspace)
    {
        return Err(refused(code!("machine.not_found"), false));
    }
    let theirs = shared
        .db
        .has_machine(&ask.asker.account, &machine)
        .await
        .map_err(Refusal::Failed)?;
    if !theirs {
        return Err(refused(code!("machine.not_found"), false));
    }
    let relayed = shared.links.next_id();
    shared.relay.wait(relayed, asked, envelope.id, back)?;
    let sent = Envelope {
        id: relayed,
        message,
    };
    if shared.links.send(&machine, &needs, sent).is_err() {
        shared.relay.lock().remove(&relayed);
        return Err(refused(code!("machine.offline"), true));
    }
    Ok(())
}

impl Default for Relay {
    fn default() -> Self {
        Self::new(MOST_WAITING, ANSWER_WITHIN)
    }
}

impl Relay {
    /// A relay where at most `most_waiting` asks wait, each for at most `answer_within`.
    pub(crate) fn new(most_waiting: usize, answer_within: Duration) -> Self {
        Self {
            waiting: Mutex::default(),
            most_waiting,
            answer_within,
        }
    }

    /// Takes `envelope` from `machine`'s connection: an answer, or an error, to an ask the
    /// relay forwarded to that machine goes back to the connection that asked. Anything else,
    /// or an answer that names another machine, is dropped. False when its id isn't one of
    /// the relay's, so another feature may claim it.
    #[must_use]
    pub(crate) fn answered(&self, machine: &MachineId, envelope: Envelope) -> bool {
        let mut waiting = self.lock();
        let Entry::Occupied(entry) = waiting.entry(envelope.id) else {
            return false;
        };
        let named = answering(&envelope.message);
        let asked = &entry.get().asked;
        let answers = match &named {
            Some(named) => named == asked,
            None => matches!(envelope.message, Message::Error(_)),
        };
        if asked.machine != *machine || !answers {
            return true;
        }
        // Told before the lock goes, as in `expire`.
        entry.remove().tell(envelope.message);
        true
    }

    /// Waits, under `relayed`, for the answer to what the asker `asked` under `id`.
    fn wait(
        self: &Arc<Self>,
        relayed: u32,
        asked: Asked,
        id: u32,
        back: &mpsc::Sender<Envelope>,
    ) -> Result<(), Refusal> {
        let mut waiting = self.lock();
        waiting.retain(|_, waiter| !waiter.back.is_closed());
        let theirs = waiting
            .values()
            .filter(|waiter| waiter.back.same_channel(back))
            .count();
        // Within the asker's room, so every answer fits.
        if waiting.len() >= self.most_waiting || theirs >= back.capacity() {
            return Err(refused(code!("relay.busy"), true));
        }
        let Entry::Vacant(entry) = waiting.entry(relayed) else {
            return Err(refused(code!("relay.busy"), true));
        };
        let relay = Arc::clone(self);
        let timer = tokio::spawn(async move {
            sleep(relay.answer_within).await;
            relay.expire(relayed);
        });
        entry.insert(Waiter {
            asked,
            id,
            back: back.clone(),
            timer: timer.abort_handle(),
        });
        Ok(())
    }

    /// Tells the ask under `relayed`, if it still waits, that no answer came.
    fn expire(&self, relayed: u32) {
        let mut waiting = self.lock();
        // Told before the lock goes, so no ask is let in ahead of the answer it takes room for.
        if let Some(waiter) = waiting.remove(&relayed) {
            waiter.tell(Message::Error(ErrorBody {
                code: code!("machine.no_answer"),
                retryable: true,
            }));
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u32, Waiter>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

const fn refused(code: Code, retryable: bool) -> Refusal {
    Refusal::Refused(ErrorBody { code, retryable })
}

/// What a message the relay carries asks, with `by` stamped on it whatever the client
/// put there; `None` for a kind the relay doesn't carry.
fn stamped(mut message: Message, by: &Asker) -> Option<(Asked, Message)> {
    let asked = if let Message::MachineSettingsQuery(query) = &mut message {
        query.by = Some(by.clone());
        Asked {
            kind: SETTINGS,
            machine: query.machine.clone(),
            workspace: Some(query.workspace.clone()),
            request: query.request.clone(),
        }
    } else if let Message::MachineSettingsCommand(command) = &mut message {
        command.by = Some(by.clone());
        Asked {
            kind: SETTINGS,
            machine: command.machine.clone(),
            workspace: Some(command.workspace.clone()),
            request: command.request.clone(),
        }
    } else if let Message::SessionsQuery(query) = &mut message {
        // No workspace, so no member: only the account.
        query.by = Some(by.account.clone());
        Asked {
            kind: SESSIONS,
            machine: query.machine.clone(),
            workspace: None,
            request: query.request.clone(),
        }
    } else {
        return None;
    };
    Some((asked, message))
}

/// What an answer the relay carries back names.
fn answering(message: &Message) -> Option<Asked> {
    if let Message::MachineSettingsAnswer(answer) = message {
        return Some(Asked {
            kind: SETTINGS,
            machine: answer.machine.clone(),
            workspace: Some(answer.workspace.clone()),
            request: answer.request.clone(),
        });
    }
    if let Message::SessionsAnswer(answer) = message {
        return Some(Asked {
            kind: SESSIONS,
            machine: answer.machine.clone(),
            workspace: None,
            request: answer.request.clone(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::task::yield_now;
    use tokio::time::timeout;

    use weacon_protocol::{
        AccessLevel, AccountId, ErrorBody, MachineSettings, MachineSettingsAction,
        MachineSettingsAnswer, MachineSettingsAnswerKind, MachineSettingsCommand,
        MachineSettingsQuery, MachineSettingsQueryKind, MemberId, RequestId,
    };
    use weacon_protocol::{
        AgentKind, AgentSession, OutsideSession, SessionsAnswer, SessionsAnswerKind, SessionsQuery,
        SessionsQueryKind,
    };

    use super::*;
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
                sign_ins: Vec::new(),
            }),
        };
        change(&mut answer);
        Message::MachineSettingsAnswer(answer)
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
        let mut fixture = Fixture::new().await;
        fixture.quiet();
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
        w1.unlink(&member_id("m1")).await;

        send(&mut browser, envelope(1, query(&paired.machine_id(), "w1"))).await;

        assert!(ends(&mut browser).await);
        assert!(fixture.shared.relay.lock().is_empty());
        drop(core);
    }

    #[tokio::test]
    async fn a_member_who_leaves_while_their_machine_answers_hears_nothing_more() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();
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
        w1.unwrap().unlink(&member_id("m1")).await;
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

    // A relay where `machine` was asked under 41, relayed as 7; and where its answer goes.
    fn waiting_on(machine: &MachineId) -> (Relay, mpsc::Receiver<Envelope>) {
        waiting_for(SETTINGS, machine, Some(workspace_id("w1")))
    }

    // A relay where an ask of `kind` waits, under 7, for `machine`'s answer in `workspace`.
    fn waiting_for(
        kind: Code,
        machine: &MachineId,
        workspace: Option<WorkspaceId>,
    ) -> (Relay, mpsc::Receiver<Envelope>) {
        let relay = Relay::new(MOST_WAITING, ANSWER_WITHIN);
        let (back, answers) = mpsc::channel(4);
        let waiter = Waiter {
            asked: Asked {
                kind,
                machine: machine.clone(),
                workspace,
                request: RequestId::try_from("r-1".to_owned()).unwrap(),
            },
            id: 41,
            back,
            timer: tokio::spawn(async {}).abort_handle(),
        };
        relay.lock().insert(7, waiter);
        (relay, answers)
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
            relay.answered(&one, envelope(7, sessions_answer(&one, "/work"))),
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
    async fn a_sessions_ask_takes_only_its_own_machines_sessions_answer() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let two = MachineId::try_from("mc-2".to_owned()).unwrap();
        let (relay, mut answers) = waiting_for(SESSIONS, &one, None);

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

    #[tokio::test]
    async fn an_answer_this_server_cant_send_on_is_refused_to_the_asker() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let (relay, mut answers) = waiting_for(SESSIONS, &one, None);
        let Message::SessionsAnswer(mut answer) = sessions_answer(&one, "/work") else {
            panic!();
        };
        if let SessionsAnswerKind::List { sessions } = &mut answer.answer {
            sessions[0].agent = AgentKind::Newer;
        }

        assert!(relay.answered(&one, envelope(7, Message::SessionsAnswer(answer))));

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
        let server = fixture.run().await;
        let paired = pair_machine(fixture, server.address).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();
        let machine = paired.machine_id();
        let connected = Connected {
            machine: machine.clone(),
            owner: account.clone(),
        };
        let attached = (fixture.shared.links).open(connected, vec![Capability::from(SETTINGS)]);
        let asker = Asker {
            account,
            member: member_id("m1"),
        };
        (machine, asker, attached)
    }

    fn code_of(result: Result<(), Refusal>) -> Option<String> {
        match result {
            Ok(()) => None,
            Err(Refusal::Refused(body)) => Some(body.code.as_str().to_owned()),
            Err(Refusal::Failed(fault)) => Some(format!("failed {}", fault.code().as_str())),
        }
    }

    #[tokio::test]
    async fn each_asker_has_room_for_its_answers_and_the_server_for_its_most() {
        let mut fixture = Fixture::new().await;
        fixture.shared.relay = Arc::new(Relay::new(3, ANSWER_WITHIN));
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = workspace_id("w1");
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };
        let ((one, one_answers), (two, _two_answers)) = (mpsc::channel(1), mpsc::channel(4));
        let three = mpsc::channel(4).0;
        let to = |id| envelope(id, query(&machine, "w1"));

        let mut codes = Vec::new();
        for (id, back) in [(1, &one), (2, &one), (3, &two), (4, &two), (5, &three)] {
            codes.push(code_of(forward(&fixture.shared, &ask, to(id), back).await));
        }
        drop(one_answers);
        // An asker that left no longer holds its place.
        codes.push(code_of(forward(&fixture.shared, &ask, to(6), &three).await));

        let busy = Some("relay.busy".to_owned());
        assert_eq!(codes, [None, busy.clone(), None, None, busy, None]);
        assert_eq!(fixture.shared.relay.lock().len(), 3);
    }

    #[tokio::test]
    async fn an_ask_unanswered_in_time_is_told_so() {
        let mut fixture = Fixture::new().await;
        fixture.shared.relay = Arc::new(Relay::new(MOST_WAITING, Duration::ZERO));
        let (machine, asker, _attached) = adas(&fixture).await;
        let w1 = workspace_id("w1");
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
        let w1 = workspace_id("w1");
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
        let w1 = workspace_id("w1");
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
        let w1 = workspace_id("w1");
        let ask = Ask {
            asker: &asker,
            workspace: &w1,
        };

        let sent = envelope(1, settings_answer(&machine));
        let result = forward(&fixture.shared, &ask, sent, &mpsc::channel(4).0).await;

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
        let w1 = workspace_id("w1");
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
}
