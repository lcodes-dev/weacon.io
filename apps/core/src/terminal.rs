//! Sign-in terminals: the vendor's login on this machine, shown sealed in its owner's browser.
//! Only the machine's owner may ask for one, and nothing that passes through one is read,
//! kept or logged.

mod seal;
mod session;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use tokio::sync::mpsc;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AccountId, AgentKind, Bytes, Code, MachineId, TerminalAction, TerminalCommand, TerminalEvent,
    TerminalId, TerminalInput, TerminalUpdate, WorkspaceId, code,
};
use x25519_dalek::StaticSecret;

use crate::workspaces::Workspaces;
use session::Session;

// Frames waiting for a terminal that hasn't taken them yet.
const WAITING: usize = 64;

/// Who this machine is: its id, its owner and the private half of its key.
pub(crate) struct Paired {
    pub(crate) machine: MachineId,
    pub(crate) owner: AccountId,
    pub(crate) key: StaticSecret,
}

/// This machine's sign-in terminals, which it answers its owner's terminal commands with.
pub(crate) struct Terminals {
    paired: Paired,
    workspaces: Workspaces,
    log: Logger,
    updates: mpsc::Sender<TerminalUpdate>,
    // A terminal id is never taken twice, so a handshake can't be played again.
    seen: HashSet<TerminalId>,
    open: HashMap<TerminalId, Open>,
}

// A terminal until its session has ended: closing it drops `frames` but keeps its slot.
struct Open {
    workspace: WorkspaceId,
    agent: AgentKind,
    frames: Option<mpsc::Sender<Bytes>>,
    alive: Weak<()>,
}

impl Terminals {
    /// What they send goes to `updates`.
    pub(crate) fn new(
        paired: Paired,
        workspaces: Workspaces,
        log: Logger,
        updates: mpsc::Sender<TerminalUpdate>,
    ) -> Self {
        Self {
            paired,
            workspaces,
            log,
            updates,
            seen: HashSet::new(),
            open: HashMap::new(),
        }
    }

    /// Answers one command, from the owner of this machine only.
    pub(crate) async fn handle(&mut self, command: TerminalCommand) {
        let asked = command.machine == self.paired.machine
            && command
                .by
                .as_ref()
                .is_some_and(|by| by.account == self.paired.owner);
        if !asked {
            return self.refuse(command, code!("terminal.refused")).await;
        }
        self.open.retain(|_, open| open.alive.strong_count() > 0);
        let open = self.open.get_mut(&command.terminal);
        let ours = open.filter(|open| open.workspace == command.workspace);
        match (&command.action, ours) {
            (TerminalAction::Open { agent, handshake }, None) => {
                let (agent, handshake) = (*agent, handshake.clone());
                self.start(command, agent, &handshake).await;
            }
            // A frame that finds the terminal's queue full is dropped, so the next one fails
            // to open and the terminal closes as any other does.
            (TerminalAction::Frame { sealed }, Some(open)) => {
                if let Some(frames) = &open.frames {
                    drop(frames.try_send(sealed.clone()));
                }
            }
            (TerminalAction::Close, Some(open)) => open.frames = None,
            // Answering would tell the browser its terminal closed while it still runs.
            (TerminalAction::Open { .. }, Some(_)) => {}
            _ => self.refuse(command, code!("terminal.unknown")).await,
        }
    }

    async fn start(&mut self, command: TerminalCommand, agent: AgentKind, handshake: &Bytes) {
        if !self.seen.insert(command.terminal.clone()) {
            return self.refuse(command, code!("terminal.refused")).await;
        }
        let busy = self
            .open
            .values()
            .any(|open| open.workspace == command.workspace && open.agent == agent);
        if busy {
            return self.refuse(command, code!("terminal.busy")).await;
        }
        let Some(prologue) = command.prologue() else {
            return self.refuse(command, code!("terminal.unknown")).await;
        };
        let log = self.log.child(&ids(&command.terminal, &command.workspace));
        let session = match self.session(&command, agent, handshake, &prologue) {
            Ok(session) => session,
            Err(fault) => {
                log.warn("terminal refused", &[("err", Field::error(&fault))]);
                return self.refuse(command, fault.code().clone()).await;
            }
        };
        let (frames, received) = mpsc::channel(WAITING);
        let alive = Arc::new(());
        log.info(
            "terminal opened",
            &[("agent", agent.name().unwrap_or("").into())],
        );
        let open = Open {
            workspace: command.workspace,
            agent,
            frames: Some(frames),
            alive: Arc::downgrade(&alive),
        };
        tokio::spawn(session::run(
            session,
            received,
            alive,
            self.updates.clone(),
            log,
        ));
        self.open.insert(command.terminal, open);
    }

    fn session(
        &self,
        command: &TerminalCommand,
        agent: AgentKind,
        handshake: &Bytes,
        prologue: &[u8],
    ) -> Result<Session, Fault> {
        let accepted = seal::accept(self.paired.key.as_bytes(), prologue, handshake.as_slice())?;
        let Ok(TerminalInput::Resize { cols, rows }) = TerminalInput::from_json(&accepted.first)
        else {
            return Err(session::size_refused());
        };
        let size = session::size(cols, rows)?;
        Ok(Session {
            terminal: command.terminal.clone(),
            workspace: command.workspace.clone(),
            launch: self.workspaces.sign_in(&command.workspace, agent)?,
            seal: accepted.seal,
            size,
            reply: Bytes::new(accepted.reply).ok_or_else(seal::seal_failed)?,
        })
    }

    async fn refuse(&self, command: TerminalCommand, code: Code) {
        let closed = TerminalUpdate {
            terminal: command.terminal,
            workspace: command.workspace,
            event: TerminalEvent::Closed { code: Some(code) },
        };
        drop(self.updates.send(closed).await);
    }
}

fn ids(terminal: &TerminalId, workspace: &WorkspaceId) -> [(&'static str, Field); 2] {
    [
        ("terminal_id", terminal.as_str().into()),
        ("workspace_id", workspace.as_str().into()),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tokio::time::timeout;
    use weacon_protocol::{Asker, MemberId, SignInState, TerminalOutput};
    use weacon_testing::TempDir;
    use x25519_dalek::PublicKey;

    use super::*;
    use snow::{HandshakeState, TransportState};

    use crate::terminal::seal::tests::{finish, initiator, opened, sealed, write};
    use crate::test_support;

    const PRINTED: &str = "MARKER-PRINTED-9d1e";
    const TYPED: &str = "MARKER-TYPED-77c4";

    // A `claude` that asks for a code, says what it got, and is signed in once asked.
    fn plant_claude(bin: &Path, ran: &Path) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1 $2\" = \"auth status\" ]; then printf '{{\"authMethod\":\"claude.ai\"}}'; exit 0; fi\n\
             touch '{}'\n\
             echo 'Paste the code {PRINTED}'\n\
             IFS= read -r code\n\
             echo \"Got $code\"\n",
            ran.display()
        );
        crate::platform::write_program(&bin.join("claude"), &script);
    }

    struct Machine {
        dir: TempDir,
        terminals: Terminals,
        updates: mpsc::Receiver<TerminalUpdate>,
        public: Vec<u8>,
        records: weacon_testing::Records,
    }

    fn machine() -> Machine {
        let dir = TempDir::new().unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        plant_claude(&bin, &dir.join("ran"));
        let key = StaticSecret::from([7; 32]);
        let public = PublicKey::from(&key).as_bytes().to_vec();
        let workspaces = Workspaces::new(&dir.join("state"), Some(bin.into_os_string()));
        let (log, records) = test_support::recording_logger();
        let (sender, updates) = mpsc::channel(64);
        let paired = Paired {
            machine: id("m-1"),
            owner: id("a-owner"),
            key,
        };
        let terminals = Terminals::new(paired, workspaces, log, sender);
        Machine {
            dir,
            terminals,
            updates,
            public,
            records,
        }
    }

    fn id<T: TryFrom<String, Error = String>>(text: &str) -> T {
        T::try_from(text.to_owned()).unwrap()
    }

    fn command(terminal: &str, action: TerminalAction) -> TerminalCommand {
        TerminalCommand {
            terminal: id(terminal),
            machine: id("m-1"),
            workspace: id("w-1"),
            action,
            by: Some(Asker {
                account: id("a-owner"),
                member: id::<MemberId>("p-1"),
            }),
        }
    }

    fn bytes(raw: Vec<u8>) -> Bytes {
        Bytes::new(raw).unwrap()
    }

    // The browser's handshake for `terminal`, sealed to `remote` with `payload`, and the
    // command that carries it.
    fn sealed_to(
        remote: &[u8],
        terminal: &str,
        payload: &TerminalInput,
    ) -> (HandshakeState, TerminalCommand) {
        let open = |handshake| {
            command(
                terminal,
                TerminalAction::Open {
                    agent: AgentKind::Claude,
                    handshake: bytes(handshake),
                },
            )
        };
        let prologue = open(Vec::new()).prologue().unwrap();
        let mut browser = initiator(&prologue, remote, None);
        let first = write(&mut browser, &payload.to_json().unwrap());
        (browser, open(first))
    }

    fn opening(machine: &Machine, terminal: &str) -> (HandshakeState, TerminalCommand) {
        sealed_to(
            &machine.public,
            terminal,
            &TerminalInput::Resize { cols: 80, rows: 24 },
        )
    }

    async fn next_update(machine: &mut Machine) -> TerminalUpdate {
        let update = timeout(Duration::from_secs(30), machine.updates.recv());
        update.await.unwrap().unwrap()
    }

    async fn next(machine: &mut Machine) -> TerminalEvent {
        next_update(machine).await.event
    }

    fn frame(browser: &mut TransportState, input: &TerminalInput) -> TerminalAction {
        TerminalAction::Frame {
            sealed: bytes(sealed(browser, &input.to_json().unwrap())),
        }
    }

    // What the terminal showed until its login ended, and the sign-in it ended with.
    async fn shown_until_ended(
        machine: &mut Machine,
        browser: &mut TransportState,
        relayed: &mut String,
    ) -> (String, SignInState) {
        let mut shown = String::new();
        loop {
            let update = next_update(machine).await;
            relayed.push_str(&serde_json::to_string(&update).unwrap());
            let TerminalEvent::Frame { sealed } = update.event else {
                panic!("{:?} after {shown:?}", update.event);
            };
            match TerminalOutput::from_json(&opened(browser, sealed.as_slice())).unwrap() {
                TerminalOutput::Output { data } => {
                    shown.push_str(&String::from_utf8_lossy(data.as_slice()));
                }
                TerminalOutput::Ended { sign_in } => return (shown, sign_in),
                TerminalOutput::Newer => {}
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_login_runs_sealed_and_neither_the_relay_nor_the_logs_see_what_passes() {
        let mut machine = machine();
        let mut relayed = String::new();
        let (handshaking, open) = opening(&machine, "t-1");
        relayed.push_str(&serde_json::to_string(&open).unwrap());
        machine.terminals.handle(open).await;
        let TerminalEvent::Accepted { handshake } = next(&mut machine).await else {
            panic!("not accepted");
        };
        let mut browser = finish(handshaking, handshake.as_slice());
        let typed = TerminalInput::Input {
            data: bytes(format!("{TYPED}\r").into_bytes()),
        };
        let input = command("t-1", frame(&mut browser, &typed));
        relayed.push_str(&serde_json::to_string(&input).unwrap());
        machine.terminals.handle(input).await;

        let (shown, sign_in) = shown_until_ended(&mut machine, &mut browser, &mut relayed).await;
        assert_eq!(sign_in, SignInState::SignedIn);
        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
        assert!(
            shown.contains(PRINTED) && shown.contains(&format!("Got {TYPED}")),
            "{shown}"
        );

        let logged = format!("{:?}", machine.records.lock().unwrap());
        for marker in [PRINTED, TYPED] {
            assert!(!relayed.contains(marker), "the relay saw {marker}");
            // Bytes travel as hex, so unsealed ones would show there as hex.
            let hex = serde_json::to_string(&bytes(marker.as_bytes().to_vec())).unwrap();
            assert!(
                !relayed.contains(hex.trim_matches('"')),
                "the relay saw {marker} as hex"
            );
            assert!(!logged.contains(marker), "a log saw {marker}");
        }
        assert!(
            logged.contains("terminal opened") && logged.contains("terminal closed"),
            "{logged}"
        );
    }

    fn closed(code: &'static str) -> TerminalEvent {
        TerminalEvent::Closed {
            code: Some(Code::new(code)),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_the_machines_owner_may_open_a_terminal_or_send_it_anything() {
        let mut machine = machine();
        let (_browser, open) = opening(&machine, "t-1");
        let strangers = [
            TerminalCommand {
                by: None,
                ..open.clone()
            },
            TerminalCommand {
                by: Some(Asker {
                    account: id("a-other"),
                    member: id::<MemberId>("p-2"),
                }),
                ..open.clone()
            },
            TerminalCommand {
                machine: id("m-2"),
                ..open.clone()
            },
        ];
        for stranger in strangers {
            machine.terminals.handle(stranger).await;
            assert_eq!(next(&mut machine).await, closed("terminal.refused"));
        }
        machine
            .terminals
            .handle(command("t-9", TerminalAction::Close))
            .await;
        assert_eq!(next(&mut machine).await, closed("terminal.unknown"));
        assert!(!machine.dir.join("ran").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_handshake_starts_nothing_until_its_browser_seals_a_frame() {
        let mut machine = machine();
        let (_browser, open) = opening(&machine, "t-1");
        machine.terminals.handle(open.clone()).await;
        assert!(matches!(
            next(&mut machine).await,
            TerminalEvent::Accepted { .. }
        ));
        machine
            .terminals
            .handle(command("t-1", TerminalAction::Close))
            .await;
        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
        assert!(!machine.dir.join("ran").exists());

        // The same handshake again, or any terminal id seen before, is refused.
        machine.terminals.handle(open).await;
        assert_eq!(next(&mut machine).await, closed("terminal.refused"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn one_sign_in_runs_per_workspace_and_vendor_at_a_time() {
        let mut machine = machine();
        let (_first, open_one) = opening(&machine, "t-1");
        machine.terminals.handle(open_one).await;
        assert!(matches!(
            next(&mut machine).await,
            TerminalEvent::Accepted { .. }
        ));
        let (_second, open_two) = opening(&machine, "t-2");
        machine.terminals.handle(open_two).await;
        assert_eq!(next(&mut machine).await, closed("terminal.busy"));

        machine
            .terminals
            .handle(command("t-1", TerminalAction::Close))
            .await;
        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
        let (_third, open_three) = opening(&machine, "t-3");
        machine.terminals.handle(open_three).await;
        assert!(matches!(
            next(&mut machine).await,
            TerminalEvent::Accepted { .. }
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_handshake_for_another_key_or_size_is_refused() {
        let mut machine = machine();
        let other = PublicKey::from(&StaticSecret::from([9; 32]))
            .as_bytes()
            .to_vec();
        let resize = TerminalInput::Resize { cols: 80, rows: 24 };
        let (_elsewhere, elsewhere) = sealed_to(&other, "t-1", &resize);
        machine.terminals.handle(elsewhere).await;
        assert_eq!(
            next(&mut machine).await,
            closed("terminal.handshake_failed")
        );

        for (terminal, payload) in [
            ("t-2", TerminalInput::Resize { cols: 0, rows: 24 }),
            (
                "t-3",
                TerminalInput::Input {
                    data: bytes(b"y".to_vec()),
                },
            ),
        ] {
            let (_browser, open) = sealed_to(&machine.public, terminal, &payload);
            machine.terminals.handle(open).await;
            assert_eq!(next(&mut machine).await, closed("terminal.size_refused"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_tampered_frame_closes_the_terminal_before_its_login_starts() {
        let mut machine = machine();
        let (handshaking, open) = opening(&machine, "t-1");
        machine.terminals.handle(open).await;
        let TerminalEvent::Accepted { handshake } = next(&mut machine).await else {
            panic!("not accepted");
        };
        let mut browser = finish(handshaking, handshake.as_slice());
        let mut tampered = sealed(&mut browser, b"{}");
        if let Some(byte) = tampered.first_mut() {
            *byte ^= 1;
        }
        let action = TerminalAction::Frame {
            sealed: bytes(tampered),
        };
        machine.terminals.handle(command("t-1", action)).await;
        assert_eq!(next(&mut machine).await, closed("terminal.seal_failed"));
        assert!(!machine.dir.join("ran").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn opening_a_live_terminal_again_changes_nothing() {
        let mut machine = machine();
        let (_browser, open) = opening(&machine, "t-1");
        machine.terminals.handle(open.clone()).await;
        assert!(matches!(next(&mut machine).await, TerminalEvent::Accepted { .. }));
        machine.terminals.handle(open).await;
        machine.terminals.handle(command("t-1", TerminalAction::Close)).await;
        machine.terminals.handle(command("t-1", TerminalAction::Close)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Closed { code: None });
        machine.updates.try_recv().unwrap_err();
    }

    // Time is paused, so the deadline passes as soon as nothing else can happen.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_browser_that_never_seals_a_frame_loses_its_terminal() {
        let mut machine = machine();
        let (_browser, open) = opening(&machine, "t-1");
        machine.terminals.handle(open).await;
        let accepted = machine.updates.recv().await.unwrap();
        assert!(matches!(accepted.event, TerminalEvent::Accepted { .. }));
        let started = tokio::time::Instant::now();
        let ended = machine.updates.recv().await.unwrap();
        assert_eq!(ended.event, closed("terminal.timed_out"));
        assert_eq!(started.elapsed(), Duration::from_mins(1));
        assert!(!machine.dir.join("ran").exists());
    }
}
