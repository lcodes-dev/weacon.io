//! Asks one of a person's machines for something over its open connection, and takes the
//! machine's answer back to the connection that asked.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use weacon_observability::Fault;
use weacon_protocol::{
    Asker, Capability, Code, Envelope, ErrorBody, MachineId, Message, WorkspaceId, code,
};

use crate::http::Shared;

/// How many asks may wait for their answers at once, server-wide.
const MOST_WAITING: usize = 4096;
/// How long an ask waits for its answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// What a machine's core offers, and the server offers, to have its settings relayed.
pub(crate) const SETTINGS: Code = code!("machine.settings");

/// The asks waiting for their machines' answers.
pub(crate) struct Relay {
    inner: Mutex<Inner>,
    most_waiting: usize,
    answer_within: Duration,
}

#[derive(Default)]
struct Inner {
    waiting: HashMap<u32, Waiter>,
    next_id: u32,
}

/// An ask's way back to the connection that asked.
struct Waiter {
    machine: MachineId,
    id: u32,
    back: mpsc::Sender<Envelope>,
}

impl Waiter {
    // The asker that left, or reads too slowly, misses it.
    fn tell(self, message: Message) {
        drop(self.back.try_send(Envelope {
            id: self.id,
            message,
        }));
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

/// Forwards `envelope` to the machine its message names, if the message is for the asker's
/// workspace and the machine is the asker's own. The answer reaches `back` under the
/// envelope's id, or `machine.no_answer` once it has waited too long.
///
/// # Errors
/// `relay.unknown` for a kind the relay doesn't carry, `machine.not_found` for a machine that
/// isn't the asker's or another workspace, `machine.offline` and `relay.busy`, which may pass,
/// and the database's failure.
pub(crate) async fn forward(
    shared: &Shared,
    ask: &Ask<'_>,
    envelope: Envelope,
    back: &mpsc::Sender<Envelope>,
) -> Result<(), Refusal> {
    let (machine, workspace, message) =
        stamped(envelope.message, ask.asker).ok_or(refused(code!("relay.unknown"), false))?;
    if workspace != *ask.workspace {
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
    let offline = || refused(code!("machine.offline"), true);
    if !shared.links.offers(&machine, &Capability::from(SETTINGS)) {
        return Err(offline());
    }
    let relay = &shared.relay;
    let relayed = relay.wait(machine.clone(), envelope.id, back)?;
    if shared
        .links
        .send(
            &machine,
            Envelope {
                id: relayed,
                message,
            },
        )
        .is_err()
    {
        relay.lock().waiting.remove(&relayed);
        return Err(offline());
    }
    let relay = Arc::clone(relay);
    tokio::spawn(async move {
        sleep(relay.answer_within).await;
        relay.expire(relayed);
    });
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
            inner: Mutex::default(),
            most_waiting,
            answer_within,
        }
    }

    /// Takes `envelope` from `machine`'s connection: an answer, or an error, to an ask the
    /// relay forwarded to that machine goes back to the connection that asked. Anything else,
    /// or an answer that names another machine, is dropped.
    pub(crate) fn answered(&self, machine: &MachineId, envelope: Envelope) {
        let answers = match answering(&envelope.message) {
            Some(named) => named == machine,
            None => matches!(envelope.message, Message::Error(_)),
        };
        if !answers {
            return;
        }
        let mut inner = self.lock();
        let Entry::Occupied(entry) = inner.waiting.entry(envelope.id) else {
            return;
        };
        if entry.get().machine != *machine {
            return;
        }
        entry.remove().tell(envelope.message);
    }

    /// Waits for `machine`'s answer to the asker's `id`, under the id it returns.
    fn wait(
        &self,
        machine: MachineId,
        id: u32,
        back: &mpsc::Sender<Envelope>,
    ) -> Result<u32, Refusal> {
        let mut inner = self.lock();
        inner.waiting.retain(|_, waiter| !waiter.back.is_closed());
        let theirs = inner
            .waiting
            .values()
            .filter(|waiter| waiter.back.same_channel(back))
            .count();
        // Within the asker's room, so every answer fits.
        if inner.waiting.len() >= self.most_waiting || theirs >= back.capacity() {
            return Err(refused(code!("relay.busy"), true));
        }
        inner.next_id = inner.next_id.wrapping_add(1);
        let relayed = inner.next_id;
        let waiter = Waiter {
            machine,
            id,
            back: back.clone(),
        };
        inner.waiting.insert(relayed, waiter);
        Ok(relayed)
    }

    /// Tells the ask under `relayed`, if it still waits, that no answer came.
    fn expire(&self, relayed: u32) {
        let waiter = self.lock().waiting.remove(&relayed);
        if let Some(waiter) = waiter {
            waiter.tell(Message::Error(ErrorBody {
                code: code!("machine.no_answer"),
                retryable: true,
            }));
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

const fn refused(code: Code, retryable: bool) -> Refusal {
    Refusal::Refused(ErrorBody { code, retryable })
}

/// The machine and workspace a message the relay carries names, with `by` stamped as the
/// asker whatever the client put there; `None` for a kind the relay doesn't carry.
fn stamped(mut message: Message, asker: &Asker) -> Option<(MachineId, WorkspaceId, Message)> {
    let (machine, workspace) = if let Message::MachineSettingsQuery(query) = &mut message {
        query.by = Some(asker.clone());
        (query.machine.clone(), query.workspace.clone())
    } else if let Message::MachineSettingsCommand(command) = &mut message {
        command.by = Some(asker.clone());
        (command.machine.clone(), command.workspace.clone())
    } else {
        return None;
    };
    Some((machine, workspace, message))
}

/// The machine an answer the relay carries back names.
fn answering(message: &Message) -> Option<&MachineId> {
    if let Message::MachineSettingsAnswer(answer) = message {
        return Some(&answer.machine);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::time::timeout;

    use weacon_protocol::{
        AccessLevel, AccountId, ErrorBody, MachineSettings, MachineSettingsAnswer,
        MachineSettingsAnswerKind, MachineSettingsQuery, MachineSettingsQueryKind, MemberId,
        RequestId,
    };

    use super::*;
    use crate::links::{Attached, Connected, QUEUE};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Wire;

    use crate::test_support::socket::{Client, ends, next, next_envelope, open, say, send};
    use crate::test_support::{Fixture, Paired, ask, member_id, pair_machine, run, workspace_id};

    fn machine_of(paired: &Paired) -> MachineId {
        MachineId::try_from(paired.machine["id"].as_str().unwrap().to_owned()).unwrap()
    }

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

    fn settings_answer(machine: &MachineId) -> Message {
        Message::MachineSettingsAnswer(MachineSettingsAnswer {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            machine: machine.clone(),
            workspace: workspace_id("w1"),
            answer: MachineSettingsAnswerKind::Current(MachineSettings {
                reports: None,
                access: AccessLevel::AskFirst,
                team: false,
                sign_ins: Vec::new(),
            }),
        })
    }

    // Once `client`'s peer has read all it was sent: it reads in order, and pongs as it does.
    async fn synced(client: &mut Client) {
        client.send(Wire::Ping(Vec::new().into())).await.unwrap();
        while !matches!(client.next().await, Some(Ok(Wire::Pong(_)))) {}
    }

    // `paired`'s core, connected and offering `capabilities`, once its connection is relayed to.
    async fn connected(address: SocketAddr, paired: &Paired, capabilities: &[&str]) -> Client {
        let mut core = open(address, "/api/machines/connect", &paired.token)
            .await
            .unwrap();
        say(&mut core, capabilities).await;
        assert!(matches!(next(&mut core).await, Message::Welcome(_)));
        synced(&mut core).await;
        core
    }

    // Ada's machine, connected as its core, and her feed of w1 offering `capabilities`.
    async fn both(
        fixture: &Fixture,
        address: SocketAddr,
        capabilities: &[&str],
    ) -> (Paired, Client, Client) {
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
    async fn asked(browser: &mut Client, id: u32, message: Message) -> Envelope {
        send(browser, envelope(id, message)).await;
        next_envelope(browser).await
    }

    fn refusal(envelope: &Envelope) -> (u32, &str, bool) {
        let Message::Error(ErrorBody { code, retryable }) = &envelope.message else {
            panic!("{envelope:?}");
        };
        (envelope.id, code.as_str(), *retryable)
    }

    #[tokio::test]
    async fn a_query_reaches_the_owners_machine_as_theirs_and_its_answer_comes_back() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (paired, mut core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        let machine = machine_of(&paired);
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;

        send(&mut browser, envelope(41, query(&machine, "w1"))).await;
        let relayed = next_envelope(&mut core).await;
        send(&mut core, envelope(relayed.id, settings_answer(&machine))).await;
        let answered = next_envelope(&mut browser).await;

        let Message::MachineSettingsQuery(query) = relayed.message else {
            panic!("{relayed:?}");
        };
        let by = query.by.unwrap();
        assert_eq!(by.account.as_str(), session["account"].as_str().unwrap());
        assert_eq!(by.member.as_str(), "m1");
        assert_ne!(relayed.id, 41);
        assert_eq!(answered, envelope(41, settings_answer(&machine)));
    }

    #[tokio::test]
    async fn a_query_for_a_machine_not_theirs_another_workspace_or_one_offline_is_refused() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (paired, core, mut browser) = both(
            &fixture,
            server.address,
            &["workspace.feed", "machine.settings"],
        )
        .await;
        let machine = machine_of(&paired);
        let stranger = MachineId::try_from("mc-stranger".to_owned()).unwrap();

        let not_theirs = asked(&mut browser, 1, query(&stranger, "w1")).await;
        let elsewhere = asked(&mut browser, 2, query(&machine, "w9")).await;
        let unknown = asked(&mut browser, 3, settings_answer(&machine)).await;
        // Ada's other machines: one whose core isn't connected, one that doesn't take settings.
        let unconnected = machine_of(&pair_machine(&fixture, server.address).await);
        let offline = asked(&mut browser, 4, query(&unconnected, "w1")).await;
        let older = pair_machine(&fixture, server.address).await;
        let _older_core = connected(server.address, &older, &["machine.connect"]).await;
        let unsettled = asked(&mut browser, 5, query(&machine_of(&older), "w1")).await;
        drop(core);

        assert_eq!(refusal(&not_theirs), (1, "machine.not_found", false));
        assert_eq!(refusal(&elsewhere), (2, "machine.not_found", false));
        assert_eq!(refusal(&unknown), (3, "relay.unknown", false));
        assert_eq!(refusal(&offline), (4, "machine.offline", true));
        assert_eq!(refusal(&unsettled), (5, "machine.offline", true));
    }

    #[tokio::test]
    async fn a_feed_whose_member_is_one_no_more_relays_nothing_and_ends() {
        let mut fixture = Fixture::new().await;
        fixture.shared.sockets.ping_every = Duration::from_hours(1);
        let server = run(fixture.shared.clone()).await;
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

        send(&mut browser, envelope(1, query(&machine_of(&paired), "w1"))).await;

        assert!(ends(&mut browser).await);
        assert!(fixture.shared.relay.lock().waiting.is_empty());
        drop(core);
    }

    #[tokio::test]
    async fn a_client_that_doesnt_offer_the_settings_relays_nothing() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (paired, core, mut browser) = both(&fixture, server.address, &["workspace.feed"]).await;
        let machine = machine_of(&paired);

        send(&mut browser, envelope(1, query(&machine, "w1"))).await;
        send(&mut browser, envelope(2, settings_answer(&machine))).await;
        synced(&mut browser).await;

        assert!(fixture.shared.relay.lock().waiting.is_empty());
        drop(core);
    }

    // A relay where `machine` was asked under 41, relayed as 7; and where its answer goes.
    fn waiting_on(machine: &MachineId) -> (Relay, mpsc::Receiver<Envelope>) {
        let relay = Relay::new(MOST_WAITING, ANSWER_WITHIN);
        let (back, answers) = mpsc::channel(4);
        let waiter = Waiter {
            machine: machine.clone(),
            id: 41,
            back,
        };
        relay.lock().waiting.insert(7, waiter);
        (relay, answers)
    }

    #[tokio::test]
    async fn only_the_asked_machine_answers_and_only_once() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let two = MachineId::try_from("mc-2".to_owned()).unwrap();
        let (relay, mut answers) = waiting_on(&one);

        relay.answered(&two, envelope(7, settings_answer(&one)));
        relay.answered(&one, envelope(7, settings_answer(&two)));
        relay.answered(&one, envelope(8, settings_answer(&one)));
        relay.answered(&one, envelope(7, Message::Unknown));
        relay.answered(&one, envelope(7, settings_answer(&one)));
        relay.answered(&one, envelope(7, settings_answer(&one)));

        assert_eq!(
            answers.recv().await.unwrap(),
            envelope(41, settings_answer(&one))
        );
        answers.try_recv().unwrap_err();
    }

    #[tokio::test]
    async fn an_error_from_the_asked_machine_comes_back() {
        let one = MachineId::try_from("mc-1".to_owned()).unwrap();
        let (relay, mut answers) = waiting_on(&one);
        let refused = Message::Error(ErrorBody {
            code: code!("settings.not_owner"),
            retryable: false,
        });

        relay.answered(&one, envelope(7, refused.clone()));

        assert_eq!(answers.recv().await.unwrap(), envelope(41, refused));
    }

    // Ada's paired machine, connected and offering settings, and her as an asker in w1.
    async fn adas(fixture: &Fixture) -> (MachineId, Asker, Attached) {
        let server = run(fixture.shared.clone()).await;
        let paired = pair_machine(fixture, server.address).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let account = AccountId::try_from(session["account"].as_str().unwrap().to_owned()).unwrap();
        let machine = machine_of(&paired);
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
        assert_eq!(fixture.shared.relay.lock().waiting.len(), 3);
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
        let told = timeout(Duration::from_secs(10), answers.recv())
            .await
            .unwrap();

        assert_eq!(code_of(sent), None);
        assert_eq!(refusal(&told.unwrap()), (1, "machine.no_answer", true));
        assert!(fixture.shared.relay.lock().waiting.is_empty());
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
        assert_eq!(fixture.shared.relay.lock().waiting.len(), QUEUE);
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
        let first = attached.incoming.recv().await.unwrap().id;
        let second = attached.incoming.recv().await.unwrap().id;
        let relay = &fixture.shared.relay;
        relay.answered(&machine, envelope(second, failed(code!("test.second"))));
        relay.answered(&machine, envelope(first, failed(code!("test.first"))));

        assert_eq!(
            refusal(&to_one.recv().await.unwrap()),
            (1, "test.first", false)
        );
        assert_eq!(
            refusal(&to_two.recv().await.unwrap()),
            (1, "test.second", false)
        );
        to_one.try_recv().unwrap_err();
        to_two.try_recv().unwrap_err();
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
        assert!(fixture.shared.relay.lock().waiting.is_empty());
    }
}
