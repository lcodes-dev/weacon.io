//! Sign-in terminals between a person's browser and one of their machines: each command goes to
//! the machine, and each of its updates back to the connection that opened the terminal. What
//! they carry is only passed on; nothing here parses, keeps or logs it.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;
use weacon_observability::Fault;
use weacon_protocol::{
    Asker, Capability, Code, Envelope, MachineId, Message, TerminalAction, TerminalCommand,
    TerminalEvent, TerminalId, TerminalUpdate, WorkspaceId, code,
};

use crate::relay::{self, Ask, Refusal};
use crate::shared::Shared;

/// What the server offers, and a browser offers back, to have sign-in terminals relayed.
pub(crate) const TERMINALS: Code = code!("terminal.relay");
/// What a machine's core offers to run sign-in terminals.
const SIGN_IN: Code = code!("terminal.sign_in");

type Key = (MachineId, TerminalId);

/// The terminals open now, each under its machine and id; held in memory only.
#[derive(Default)]
pub(crate) struct Terminals {
    open: Mutex<HashMap<Key, Route>>,
}

/// The way back to the connection that opened a terminal, and who opened it.
struct Route {
    workspace: WorkspaceId,
    by: Asker,
    back: mpsc::Sender<Envelope>,
}

/// Sends `command` to the machine it names, stamped as `ask`'s, if the machine is the asker's
/// own and the command is for the asker's workspace and a terminal this connection opened.
/// A refusal reaches `back` as the terminal's close, with its code.
///
/// # Errors
/// The database's failure.
pub(crate) async fn forward(
    shared: &Shared,
    ask: &Ask<'_>,
    mut command: TerminalCommand,
    back: &mpsc::Sender<Envelope>,
) -> Result<(), Fault> {
    match sent(shared, ask, &mut command, back).await {
        Ok(()) => Ok(()),
        Err(Refusal::Refused(error)) => {
            let closed = closed(command.terminal, command.workspace, error.code);
            drop(back.try_send(closed));
            Ok(())
        }
        Err(Refusal::Failed(fault)) => Err(fault),
    }
}

async fn sent(
    shared: &Shared,
    ask: &Ask<'_>,
    command: &mut TerminalCommand,
    back: &mpsc::Sender<Envelope>,
) -> Result<(), Refusal> {
    if command.workspace != *ask.workspace.id() {
        return Err(relay::refused(code!("machine.not_found"), false));
    }
    relay::theirs(shared, ask, &command.machine).await?;
    command.by = Some(ask.asker.clone());
    let envelope = Envelope {
        id: shared.links.next_id(),
        message: Message::TerminalCommand(command.clone()),
    };
    // A newer client's command this server can't send on would drop the machine's connection.
    if envelope.to_json().is_err() {
        return Err(relay::refused(code!("relay.unknown"), false));
    }
    let key = (command.machine.clone(), command.terminal.clone());
    shared
        .terminals
        .route(key.clone(), command, ask.asker, back)?;
    if shared
        .links
        .send(&key.0, &Capability::new(SIGN_IN), envelope)
        .is_err()
    {
        shared.terminals.lock().remove(&key);
        return Err(relay::refused(code!("machine.offline"), true));
    }
    Ok(())
}

/// Takes `update` from `machine`'s connection to the connection that opened its terminal, and
/// forgets the terminal once it is closed. An update for any other terminal is dropped.
pub(crate) fn delivered(shared: &Shared, machine: &MachineId, update: TerminalUpdate) {
    let key = (machine.clone(), update.terminal.clone());
    let mut open = shared.terminals.lock();
    let Some(route) = open
        .get(&key)
        .filter(|route| route.workspace == update.workspace)
    else {
        return;
    };
    let closing = matches!(update.event, TerminalEvent::Closed { .. });
    let message = Message::Terminal(update);
    // A browser that reads too slowly, or is gone, misses the rest, and the machine is told.
    let taken = route.back.try_send(Envelope { id: 0, message }).is_ok();
    if taken && !closing {
        return;
    }
    let Some(route) = open.remove(&key) else {
        return;
    };
    drop(open);
    if !taken {
        let lagged = closed(
            key.1.clone(),
            route.workspace.clone(),
            code!("terminal.lagged"),
        );
        tell(&route.back, lagged);
        abandon(shared, key, route);
    }
}

/// Closes each terminal open on `machine` as offline, once its connection has ended, unless a
/// newer connection of the machine has taken over.
pub(crate) fn offline(shared: &Shared, machine: &MachineId) {
    // Checked under the routes' lock, so no route opened on a newer connection is taken.
    let mut open = shared.terminals.lock();
    if shared.links.is_open(machine) {
        return;
    }
    let gone: Vec<_> = open.extract_if(|(on, _), _| on == machine).collect();
    drop(open);
    for ((_, terminal), route) in gone {
        tell(
            &route.back,
            closed(terminal, route.workspace, code!("machine.offline")),
        );
    }
}

// Sends `envelope` to a browser's connection once it has room, which it has once it has
// caught up, or not at all once it has ended.
fn tell(back: &mpsc::Sender<Envelope>, envelope: Envelope) {
    let back = back.clone();
    tokio::spawn(async move { drop(back.send(envelope).await) });
}

/// Closes each terminal the connection behind `back` opened, once that connection has ended.
pub(crate) fn ended(shared: &Shared, back: &mpsc::Sender<Envelope>) {
    let gone: Vec<_> = shared
        .terminals
        .lock()
        .extract_if(|_, route| route.back.same_channel(back))
        .collect();
    for (key, route) in gone {
        abandon(shared, key, route);
    }
}

// Asks the machine to close a terminal whose browser can no longer hear it.
fn abandon(shared: &Shared, (machine, terminal): Key, route: Route) {
    let close = TerminalCommand {
        terminal,
        machine: machine.clone(),
        workspace: route.workspace,
        action: TerminalAction::Close,
        by: Some(route.by),
    };
    let envelope = Envelope {
        id: shared.links.next_id(),
        message: Message::TerminalCommand(close),
    };
    drop(
        shared
            .links
            .send(&machine, &Capability::new(SIGN_IN), envelope),
    );
}

impl Terminals {
    // Opens a route for an open, or finds this connection's own for anything else.
    fn route(
        &self,
        key: Key,
        command: &TerminalCommand,
        by: &Asker,
        back: &mpsc::Sender<Envelope>,
    ) -> Result<(), Refusal> {
        let mut open = self.lock();
        let ours =
            |route: &Route| route.back.same_channel(back) && route.workspace == command.workspace;
        match (&command.action, open.get(&key)) {
            (TerminalAction::Open { .. }, None) => {
                let route = Route {
                    workspace: command.workspace.clone(),
                    by: by.clone(),
                    back: back.clone(),
                };
                open.insert(key, route);
                Ok(())
            }
            (TerminalAction::Open { .. }, Some(_)) => {
                Err(relay::refused(code!("terminal.refused"), false))
            }
            (_, Some(route)) if ours(route) => Ok(()),
            _ => Err(relay::refused(code!("terminal.unknown"), false)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Key, Route>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn closed(terminal: TerminalId, workspace: WorkspaceId, code: Code) -> Envelope {
    let update = TerminalUpdate {
        terminal,
        workspace,
        event: TerminalEvent::Closed { code: Some(code) },
    };
    Envelope {
        id: 0,
        message: Message::Terminal(update),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use weacon_protocol::{AccountId, Asker, Bytes, MemberId};

    use super::*;
    use crate::test_support::socket::{Socket, next, next_envelope, open, pongs, say, send};
    use crate::test_support::{Fixture, Paired, ask, pair_machine};

    const TYPED: &[u8] = b"MARKER-TYPED-c90";
    const SHOWN: &[u8] = b"MARKER-SHOWN-5d2";

    fn bytes(raw: &[u8]) -> Bytes {
        Bytes::new(raw.to_vec()).unwrap()
    }

    fn id<T: TryFrom<String, Error = String>>(text: &str) -> T {
        T::try_from(text.to_owned()).unwrap()
    }

    // A command for terminal t-1 on `machine` in `workspace`, claiming to be from someone else.
    fn command(machine: &MachineId, workspace: &str, action: TerminalAction) -> Envelope {
        Envelope {
            id: 7,
            message: Message::TerminalCommand(TerminalCommand {
                terminal: id("t-1"),
                machine: machine.clone(),
                workspace: id(workspace),
                action,
                by: Some(Asker {
                    account: id::<AccountId>("a-forged"),
                    member: id::<MemberId>("m-forged"),
                }),
            }),
        }
    }

    fn opening(machine: &MachineId, workspace: &str) -> Envelope {
        command(
            machine,
            workspace,
            TerminalAction::Open {
                agent: weacon_protocol::AgentKind::Claude,
                cols: 80,
                rows: 24,
            },
        )
    }

    fn update(event: TerminalEvent) -> Envelope {
        Envelope {
            id: 0,
            message: Message::Terminal(TerminalUpdate {
                terminal: id("t-1"),
                workspace: id("w1"),
                event,
            }),
        }
    }

    // `paired`'s core, connected and offering `capabilities`.
    async fn connect_core(address: SocketAddr, paired: &Paired, capabilities: &[&str]) -> Socket {
        let mut core = open(address, "/api/machines/connect", &paired.token)
            .await
            .unwrap();
        say(&mut core, capabilities).await;
        assert!(matches!(next(&mut core).await, Message::Welcome(_)));
        assert!(pongs(&mut core).await);
        core
    }

    // A feed of w1 for `paired`'s owner, offering the terminals.
    async fn browser(address: SocketAddr, paired: &Paired) -> Socket {
        let mut browser = open(address, "/api/workspaces/w1/feed", &paired.session)
            .await
            .unwrap();
        say(&mut browser, &["workspace.feed", "terminal.relay"]).await;
        assert!(matches!(next(&mut browser).await, Message::Welcome(_)));
        while !matches!(next(&mut browser).await, Message::Workspace(_)) {}
        browser
    }

    fn closed_code(envelope: &Envelope) -> Option<&str> {
        let Message::Terminal(TerminalUpdate {
            event: TerminalEvent::Closed { code },
            ..
        }) = &envelope.message
        else {
            panic!("{envelope:?}");
        };
        code.as_ref().map(Code::as_str)
    }

    // What a relayed command asks, and the account it was stamped with.
    fn carried(envelope: Envelope) -> (TerminalAction, String) {
        let Message::TerminalCommand(command) = envelope.message else {
            panic!("{envelope:?}");
        };
        let by = command.by.unwrap().account.as_str().to_owned();
        (command.action, by)
    }

    fn output(raw: &[u8]) -> Envelope {
        update(TerminalEvent::Output { data: bytes(raw) })
    }

    fn output_update(raw: &[u8]) -> TerminalUpdate {
        let Message::Terminal(update) = output(raw).message else {
            panic!("not an update");
        };
        update
    }

    #[tokio::test]
    async fn a_terminal_runs_between_its_browser_and_machine_with_its_bytes_untouched_and_unlogged()
    {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine_id();
        let signs_in = ["machine.connect", "terminal.sign_in"];
        let mut core = connect_core(server.address, &paired, &signs_in).await;
        let mut browser = browser(server.address, &paired).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;
        let Message::TerminalCommand(open) = opening(&machine, "w1").message else {
            panic!("not a command");
        };

        send(&mut browser, opening(&machine, "w1")).await;
        let opened = carried(next_envelope(&mut core).await);
        send(&mut core, update(TerminalEvent::Opened)).await;
        let answered = next_envelope(&mut browser).await;
        let typed = TerminalAction::Input { data: bytes(TYPED) };
        send(&mut browser, command(&machine, "w1", typed.clone())).await;
        let (passed, _) = carried(next_envelope(&mut core).await);
        send(&mut core, output(SHOWN)).await;
        let shown = next_envelope(&mut browser).await;
        send(&mut core, update(TerminalEvent::Closed { code: None })).await;
        let ended = next_envelope(&mut browser).await;

        let account = session["account"].as_str().unwrap().to_owned();
        assert_eq!(opened, (open.action, account));
        assert_eq!(answered, update(TerminalEvent::Opened));
        assert_eq!(passed, typed);
        assert_eq!(shown, output(SHOWN));
        assert_eq!(closed_code(&ended), None);
        let logged = format!("{:?}", fixture.records.lock().unwrap());
        for marker in [TYPED, SHOWN] {
            let text = String::from_utf8_lossy(marker);
            let hex = weacon_protocol::to_hex(marker);
            assert!(
                !logged.contains(&*text) && !logged.contains(&hex),
                "a log saw {text}"
            );
        }
    }

    #[tokio::test]
    async fn a_terminal_on_a_machine_not_theirs_elsewhere_or_without_sign_ins_is_closed() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let _core = connect_core(server.address, &paired, &["machine.connect"]).await;
        let mut browser = browser(server.address, &paired).await;
        let stranger: MachineId = id("mc-stranger");

        send(&mut browser, opening(&stranger, "w1")).await;
        let not_theirs = next_envelope(&mut browser).await;
        send(&mut browser, opening(&paired.machine_id(), "w9")).await;
        let elsewhere = next_envelope(&mut browser).await;
        send(&mut browser, opening(&paired.machine_id(), "w1")).await;
        let unoffered = next_envelope(&mut browser).await;

        assert_eq!(closed_code(&not_theirs), Some("machine.not_found"));
        let Message::Terminal(elsewhere_update) = &elsewhere.message else {
            panic!("{elsewhere:?}");
        };
        assert_eq!(elsewhere_update.workspace.as_str(), "w9");
        assert_eq!(closed_code(&elsewhere), Some("machine.not_found"));
        assert_eq!(closed_code(&unoffered), Some("machine.offline"));
    }

    #[tokio::test]
    async fn only_the_connection_and_machine_of_a_terminal_reach_it() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine_id();
        let mut core = connect_core(
            server.address,
            &paired,
            &["machine.connect", "terminal.sign_in"],
        )
        .await;
        let other = pair_machine(&fixture, server.address).await;
        let mut other_core = connect_core(
            server.address,
            &other,
            &["machine.connect", "terminal.sign_in"],
        )
        .await;
        let mut opener = browser(server.address, &paired).await;
        let mut second = browser(server.address, &paired).await;

        send(&mut opener, opening(&machine, "w1")).await;
        next_envelope(&mut core).await;
        send(&mut second, opening(&machine, "w1")).await;
        let reopened = next_envelope(&mut second).await;
        send(&mut second, command(&machine, "w1", TerminalAction::Close)).await;
        let not_its_own = next_envelope(&mut second).await;
        // Another machine's word for the same terminal id goes nowhere.
        send(
            &mut other_core,
            update(TerminalEvent::Closed { code: None }),
        )
        .await;
        send(&mut core, output(SHOWN)).await;
        let first_heard = next_envelope(&mut opener).await;

        assert_eq!(closed_code(&reopened), Some("terminal.refused"));
        assert_eq!(closed_code(&not_its_own), Some("terminal.unknown"));
        assert_eq!(first_heard, output(SHOWN));
    }

    #[tokio::test]
    async fn a_browser_that_goes_away_has_its_terminal_closed_on_the_machine() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine_id();
        let signs_in = ["machine.connect", "terminal.sign_in"];
        let mut core = connect_core(server.address, &paired, &signs_in).await;
        let mut browser = browser(server.address, &paired).await;
        let (_, session) = ask(server.address, "GET /api/session", &paired.proof, "").await;

        send(&mut browser, opening(&machine, "w1")).await;
        next_envelope(&mut core).await;
        drop(browser);
        let close = next_envelope(&mut core).await;

        let Message::TerminalCommand(close) = close.message else {
            panic!("{close:?}");
        };
        assert_eq!(close.action, TerminalAction::Close);
        assert_eq!(
            (close.terminal.as_str(), close.workspace.as_str()),
            ("t-1", "w1")
        );
        let by = close.by.unwrap().account;
        assert_eq!(by.as_str(), session["account"].as_str().unwrap());
    }

    #[tokio::test]
    async fn a_terminal_its_machine_closed_is_forgotten_without_a_word_back_to_the_machine() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine_id();
        let signs_in = ["machine.connect", "terminal.sign_in"];
        let mut core = connect_core(server.address, &paired, &signs_in).await;
        let mut browser = browser(server.address, &paired).await;

        send(&mut browser, opening(&machine, "w1")).await;
        next_envelope(&mut core).await;
        send(&mut core, update(TerminalEvent::Closed { code: None })).await;
        let ended = next_envelope(&mut browser).await;
        let typed = TerminalAction::Input { data: bytes(TYPED) };
        send(&mut browser, command(&machine, "w1", typed)).await;
        let late = next_envelope(&mut browser).await;

        assert_eq!(closed_code(&ended), None);
        assert_eq!(closed_code(&late), Some("terminal.unknown"));
        assert!(pongs(&mut core).await, "the machine heard more");
    }

    #[tokio::test]
    async fn a_machine_that_goes_away_has_its_terminals_closed_as_offline() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine_id();
        let signs_in = ["machine.connect", "terminal.sign_in"];
        let mut core = connect_core(server.address, &paired, &signs_in).await;
        let mut browser = browser(server.address, &paired).await;

        send(&mut browser, opening(&machine, "w1")).await;
        next_envelope(&mut core).await;
        drop(core);
        let closed = next_envelope(&mut browser).await;

        assert_eq!(closed_code(&closed), Some("machine.offline"));
    }

    #[tokio::test]
    async fn a_browser_too_slow_for_its_terminal_hears_it_closed_once_it_catches_up() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let machine: MachineId = id("mc-1");
        let (back, mut answers) = mpsc::channel(1);
        let asker = Asker {
            account: id::<AccountId>("a-1"),
            member: id::<MemberId>("m-1"),
        };
        let Message::TerminalCommand(open) = opening(&machine, "w1").message else {
            panic!("not a command");
        };
        let key = (machine.clone(), open.terminal.clone());
        shared.terminals.route(key, &open, &asker, &back).unwrap();

        delivered(shared, &machine, output_update(SHOWN));
        delivered(shared, &machine, output_update(TYPED));
        let patience = crate::test_support::PATIENCE;
        let first = tokio::time::timeout(patience, answers.recv())
            .await
            .unwrap();
        let then = tokio::time::timeout(patience, answers.recv())
            .await
            .unwrap();
        let (first, then) = (first.unwrap(), then.unwrap());

        assert_eq!(first, output(SHOWN));
        assert_eq!(closed_code(&then), Some("terminal.lagged"));
        assert!(shared.terminals.lock().is_empty());
    }

    #[tokio::test]
    async fn a_machine_connected_again_keeps_its_terminals() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let machine: MachineId = id("mc-1");
        let (back, _answers) = mpsc::channel(1);
        let asker = Asker {
            account: id::<AccountId>("a-1"),
            member: id::<MemberId>("m-1"),
        };
        let Message::TerminalCommand(open) = opening(&machine, "w1").message else {
            panic!("not a command");
        };
        let key = (machine.clone(), open.terminal.clone());
        shared.terminals.route(key, &open, &asker, &back).unwrap();
        let connected = crate::links::Connected {
            machine: machine.clone(),
            owner: asker.account,
        };
        let _again = shared.links.open(connected, Vec::new());

        offline(shared, &machine);

        assert_eq!(shared.terminals.lock().len(), 1);
    }
}
