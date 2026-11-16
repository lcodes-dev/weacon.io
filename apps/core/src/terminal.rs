//! Sign-in terminals: the vendor's login on this machine, shown in its owner's browser. The
//! uplink passes on only the owner's commands, and nothing that passes through a terminal is
//! parsed, kept or logged.

mod session;

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    AgentKind, Bytes, Code, Envelope, Message, TerminalAction, TerminalCommand, TerminalEvent,
    TerminalId, TerminalUpdate, WorkspaceId, code,
};

use crate::workspaces::Workspaces;
use session::Session;

// Inputs waiting for a terminal that hasn't taken them yet.
const WAITING: usize = 64;

/// What the uplink tells the terminals, in the order it happened.
#[derive(Debug)]
pub(crate) enum Told {
    /// The owner's command, from the server.
    Command(TerminalCommand),
    /// The connection the commands before this came on has ended.
    Down,
}

/// This machine's sign-in terminals, which it answers its owner's terminal commands with.
pub(crate) struct Terminals {
    workspaces: Workspaces,
    log: Logger,
    updates: mpsc::Sender<Envelope>,
    open: HashMap<TerminalId, Open>,
}

// A terminal until its session has ended: closing it drops `inputs` but keeps its slot.
struct Open {
    workspace: WorkspaceId,
    agent: AgentKind,
    inputs: Option<mpsc::Sender<Bytes>>,
    alive: Weak<()>,
    session: AbortHandle,
}

impl Terminals {
    /// What they send goes to `updates`.
    pub(crate) fn new(
        workspaces: Workspaces,
        log: Logger,
        updates: mpsc::Sender<Envelope>,
    ) -> Self {
        Self {
            workspaces,
            log,
            updates,
            open: HashMap::new(),
        }
    }

    /// Answers one command.
    pub(crate) async fn handle(&mut self, command: TerminalCommand) {
        self.open.retain(|_, open| open.alive.strong_count() > 0);
        let open = self.open.get_mut(&command.terminal);
        let ours = open.filter(|open| open.workspace == command.workspace);
        match (&command.action, ours) {
            (&TerminalAction::Open { agent, cols, rows }, None) => {
                self.start(command, agent, (cols, rows)).await;
            }
            // An input that finds the terminal's queue full closes it, as one lost can't be told.
            (TerminalAction::Input { data }, Some(open)) => {
                let queued = open
                    .inputs
                    .as_ref()
                    .map(|inputs| inputs.try_send(data.clone()));
                if matches!(queued, Some(Err(_))) {
                    open.inputs = None;
                }
            }
            (TerminalAction::Close, Some(open)) => open.inputs = None,
            // Answering would tell the browser its terminal closed while it still runs.
            (TerminalAction::Open { .. }, Some(_)) => {}
            _ => self.refuse(command, code!("terminal.unknown")),
        }
    }

    /// Stops every terminal's login at once, whatever it waits on, when the connection their
    /// browsers reach them through is gone; nobody is left to hear that they closed.
    pub(crate) fn close_all(&mut self) {
        self.open.values().for_each(|open| open.session.abort());
        self.open.clear();
    }

    async fn start(&mut self, command: TerminalCommand, agent: AgentKind, size: (u32, u32)) {
        let busy = self
            .open
            .values()
            .any(|open| open.workspace == command.workspace && open.agent == agent);
        if busy {
            return self.refuse(command, code!("terminal.busy"));
        }
        let log = self.log.child(&ids(&command.terminal, &command.workspace));
        let session = match self.session(&command, agent, size).await {
            Ok(session) => session,
            Err(fault) => {
                log.warn("terminal refused", &[("err", Field::error(&fault))]);
                return self.refuse(command, fault.code().clone());
            }
        };
        let (inputs, received) = mpsc::channel(WAITING);
        let alive = Arc::new(());
        log.info(
            "terminal opened",
            &[("agent", agent.name().unwrap_or("").into())],
        );
        let alive_for = Arc::downgrade(&alive);
        let running = session::run(session, received, alive, self.updates.clone(), log);
        let open = Open {
            workspace: command.workspace,
            agent,
            inputs: Some(inputs),
            alive: alive_for,
            session: tokio::spawn(running).abort_handle(),
        };
        self.open.insert(command.terminal, open);
    }

    async fn session(
        &self,
        command: &TerminalCommand,
        agent: AgentKind,
        (cols, rows): (u32, u32),
    ) -> Result<Session, Fault> {
        let size = session::size(cols, rows)?;
        let (workspaces, workspace) = (self.workspaces.clone(), command.workspace.clone());
        let launch = session::off_runtime(move || workspaces.sign_in(&workspace, agent)).await?;
        Ok(Session {
            terminal: command.terminal.clone(),
            workspace: command.workspace.clone(),
            launch,
            size,
        })
    }

    // Never waits for room: the uplink waits on these terminals when its connection ends, and
    // nothing takes what they send until the next one.
    fn refuse(&self, command: TerminalCommand, code: Code) {
        let closed = TerminalUpdate {
            terminal: command.terminal,
            workspace: command.workspace,
            event: TerminalEvent::Closed { code: Some(code) },
        };
        let message = Message::Terminal(closed);
        drop(self.updates.try_send(Envelope { id: 0, message }));
    }
}

fn ids(terminal: &TerminalId, workspace: &WorkspaceId) -> [(&'static str, Field); 2] {
    [
        ("terminal_id", terminal.as_str().into()),
        ("workspace_id", workspace.as_str().into()),
    ]
}

// The vendor these tests plant is a shell script.
#[cfg(unix)]
#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tokio::time::timeout;
    use weacon_protocol::{Asker, MemberId};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support;

    const PRINTED: &str = "MARKER-PRINTED-9d1e";
    const TYPED: &str = "MARKER-TYPED-77c4";

    // A `claude` that asks for a code and says what it got.
    const ASKS: &str =
        "echo 'Paste the code MARKER-PRINTED-9d1e'\nIFS= read -r code\necho \"Got $code\"";
    // A `claude` that takes no input and never ends.
    const STUCK: &str = "stty raw -echo\necho ready\nexec sleep 600";

    // A `claude` whose login runs `login`, and that is signed in once asked.
    fn plant_claude(bin: &Path, ran: &Path, login: &str) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1 $2\" = \"auth status\" ]; then printf '{{\"authMethod\":\"claude.ai\"}}'; exit 0; fi\n\
             touch '{}'\n\
             {login}\n",
            ran.display()
        );
        weacon_testing::platform::write_program(&bin.join("claude"), &script).unwrap();
    }

    struct Machine {
        dir: TempDir,
        terminals: Terminals,
        updates: mpsc::Receiver<Envelope>,
        records: weacon_testing::Records,
    }

    fn machine() -> Machine {
        machine_running(ASKS)
    }

    fn machine_running(login: &str) -> Machine {
        let dir = TempDir::new().unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        plant_claude(&bin, &dir.join("ran"), login);
        let workspaces = Workspaces::new(&dir.join("state"), Some(bin.into_os_string()));
        let (log, records) = test_support::recording_logger();
        // Room for every refusal a test makes before it reads any.
        let (sender, updates) = mpsc::channel(4 * WAITING);
        let terminals = Terminals::new(workspaces, log, sender);
        Machine {
            dir,
            terminals,
            updates,
            records,
        }
    }

    fn id<T: TryFrom<String, Error = String>>(text: &str) -> T {
        T::try_from(text.to_owned()).unwrap()
    }

    fn command_in(terminal: &str, workspace: &str, action: TerminalAction) -> TerminalCommand {
        TerminalCommand {
            terminal: id(terminal),
            machine: id("m-1"),
            workspace: id(workspace),
            action,
            by: Some(Asker {
                account: id("a-owner"),
                member: id::<MemberId>("p-1"),
            }),
        }
    }

    fn command(terminal: &str, action: TerminalAction) -> TerminalCommand {
        command_in(terminal, "w-1", action)
    }

    const OPEN: TerminalAction = TerminalAction::Open {
        agent: AgentKind::Claude,
        cols: 80,
        rows: 24,
    };

    fn typed(text: &[u8]) -> TerminalAction {
        TerminalAction::Input {
            data: Bytes::new(text.to_vec()),
        }
    }

    async fn event(machine: &mut Machine) -> TerminalEvent {
        let envelope = timeout(Duration::from_secs(10), machine.updates.recv());
        event_of(envelope.await.unwrap().unwrap())
    }

    // The next event that isn't output, which comes whenever a login prints.
    async fn next(machine: &mut Machine) -> TerminalEvent {
        loop {
            let event = event(machine).await;
            if !matches!(event, TerminalEvent::Output { .. }) {
                return event;
            }
        }
    }

    fn event_of(envelope: Envelope) -> TerminalEvent {
        assert_eq!(envelope.id, 0);
        let Message::Terminal(update) = envelope.message else {
            panic!("{:?}", envelope.message);
        };
        update.event
    }

    // What the terminal showed until it closed, its login having ended.
    async fn shown_until_closed(machine: &mut Machine) -> String {
        let mut shown = String::new();
        loop {
            match event(machine).await {
                TerminalEvent::Output { data } => {
                    shown.push_str(&String::from_utf8_lossy(data.as_slice()));
                }
                TerminalEvent::Closed { code: None } => return shown,
                event @ (TerminalEvent::Opened
                | TerminalEvent::Closed { .. }
                | TerminalEvent::Newer) => panic!("{event:?} after {shown:?}"),
            }
        }
    }

    fn closed(code: &'static str) -> TerminalEvent {
        TerminalEvent::Closed {
            code: Some(Code::new(code)),
        }
    }

    #[tokio::test]
    async fn the_login_runs_and_the_logs_see_nothing_that_passes() {
        let mut machine = machine();
        machine.terminals.handle(command("t-1", OPEN)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);
        let input = command("t-1", typed(format!("{TYPED}\r").as_bytes()));
        machine.terminals.handle(input).await;

        let shown = shown_until_closed(&mut machine).await;
        assert!(
            shown.contains(PRINTED) && shown.contains(&format!("Got {TYPED}")),
            "{shown}"
        );

        let logged = format!("{:?}", machine.records.lock().unwrap());
        for marker in [PRINTED, TYPED] {
            assert!(!logged.contains(marker), "a log saw {marker}");
        }
        assert!(
            logged.contains("terminal opened") && logged.contains("terminal closed"),
            "{logged}"
        );
    }

    #[tokio::test]
    async fn a_refusal_never_waits_for_room() {
        let mut machine = machine();
        let (full, _never_read) = mpsc::channel(1);
        full.try_send(Envelope {
            id: 0,
            message: Message::Unknown,
        })
        .unwrap();
        machine.terminals.updates = full;

        let refusing = machine
            .terminals
            .handle(command("t-9", TerminalAction::Close));

        timeout(Duration::from_secs(10), refusing).await.unwrap();
    }

    #[tokio::test]
    async fn a_terminal_it_doesnt_have_or_of_no_size_is_refused() {
        let mut machine = machine();
        let sizeless = TerminalAction::Open {
            agent: AgentKind::Claude,
            cols: 0,
            rows: 24,
        };

        machine
            .terminals
            .handle(command("t-9", TerminalAction::Close))
            .await;
        let unknown = next(&mut machine).await;
        machine.terminals.handle(command("t-1", sizeless)).await;
        let unsized_ = next(&mut machine).await;

        assert_eq!(unknown, closed("terminal.unknown"));
        assert_eq!(unsized_, closed("terminal.size_refused"));
        assert!(!machine.dir.join("ran").exists());
    }

    #[tokio::test]
    async fn one_sign_in_runs_per_workspace_and_vendor_at_a_time() {
        let mut machine = machine_running(STUCK);
        machine.terminals.handle(command("t-1", OPEN)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);
        machine.terminals.handle(command("t-2", OPEN)).await;
        assert_eq!(next(&mut machine).await, closed("terminal.busy"));
        machine
            .terminals
            .handle(command_in("t-4", "w-2", OPEN))
            .await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);

        machine
            .terminals
            .handle(command("t-1", TerminalAction::Close))
            .await;
        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
        machine.terminals.handle(command("t-3", OPEN)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);
    }

    #[tokio::test]
    async fn opening_a_live_terminal_again_changes_nothing() {
        let mut machine = machine_running(STUCK);
        machine.terminals.handle(command("t-1", OPEN)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);
        machine.terminals.handle(command("t-1", OPEN)).await;
        machine
            .terminals
            .handle(command("t-1", TerminalAction::Close))
            .await;
        machine
            .terminals
            .handle(command("t-1", TerminalAction::Close))
            .await;

        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
        machine.updates.try_recv().unwrap_err();
    }

    // Waits, a moment at a time, until `done`.
    async fn until(done: impl Fn() -> bool) {
        let waiting = async {
            while !done() {
                tokio::task::yield_now().await;
            }
        };
        timeout(Duration::from_secs(10), waiting).await.unwrap();
    }

    #[tokio::test]
    async fn every_login_stops_once_the_connection_is_gone_even_one_nobody_hears() {
        let mut machine = machine_running(STUCK);
        // Room for one update that nobody takes, so what the login prints waits for good.
        let (unread, mut updates) = mpsc::channel(1);
        machine.terminals.updates = unread.clone();
        machine.terminals.handle(command("t-1", OPEN)).await;
        until(|| unread.capacity() == 0).await;
        let open = machine.terminals.open.values().next().unwrap();
        let login = Weak::clone(&open.alive);

        machine.terminals.close_all();

        until(|| login.strong_count() == 0).await;
        // Its slot is free at once.
        assert_eq!(updates.try_recv().map(event_of), Ok(TerminalEvent::Opened));
        machine.terminals.handle(command("t-2", OPEN)).await;
        let next = timeout(Duration::from_secs(10), updates.recv()).await;
        assert_eq!(next.unwrap().map(event_of), Some(TerminalEvent::Opened));
    }

    #[tokio::test]
    async fn an_input_that_finds_the_queue_full_closes_the_terminal() {
        let mut machine = machine_running(STUCK);
        machine.terminals.handle(command("t-1", OPEN)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);

        // Nothing here waits, so the terminal takes none of them meanwhile.
        for _ in 0..=WAITING {
            machine.terminals.handle(command("t-1", typed(b"x"))).await;
        }

        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
    }

    #[tokio::test]
    async fn a_terminal_closes_while_its_sign_in_takes_no_input() {
        let mut machine = machine_running(STUCK);
        machine.terminals.handle(command("t-1", OPEN)).await;
        assert_eq!(next(&mut machine).await, TerminalEvent::Opened);
        assert!(matches!(
            event(&mut machine).await,
            TerminalEvent::Output { .. }
        ));

        for _ in 0..40 {
            let input = command("t-1", typed(&[b'a'; 16_384]));
            machine.terminals.handle(input).await;
        }
        let close = command("t-1", TerminalAction::Close);
        machine.terminals.handle(close).await;

        assert_eq!(
            next(&mut machine).await,
            TerminalEvent::Closed { code: None }
        );
    }
}
