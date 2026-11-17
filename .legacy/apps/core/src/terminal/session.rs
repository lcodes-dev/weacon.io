//! One sign-in terminal, from its login's start to its close.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use weacon_agents::{Event, Launch, Size, Terminal};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    Bytes, Envelope, Message, TerminalEvent, TerminalId, TerminalUpdate, WorkspaceId, code,
};

/// A terminal the core took, before its login starts.
pub(super) struct Session {
    pub(super) terminal: TerminalId,
    pub(super) workspace: WorkspaceId,
    pub(super) launch: Launch,
    pub(super) size: Size,
}

/// Runs the terminal until its login ends, `inputs` closes or anything fails, then says it is
/// closed. It holds `alive` until then.
pub(super) async fn run(
    session: Session,
    mut inputs: mpsc::Receiver<Bytes>,
    alive: Arc<()>,
    updates: mpsc::Sender<Envelope>,
    log: Logger,
) {
    let served = serve(&session, &mut inputs, &updates).await;
    // Frees the workspace's slot before the browser hears the terminal is closed.
    drop((inputs, alive));
    let code = served.err().map(|fault| {
        log.warn("terminal failed", &[("err", Field::error(&fault))]);
        fault.code().clone()
    });
    drop(send(&updates, update(&session, TerminalEvent::Closed { code })).await);
    log.info("terminal closed", &[]);
}

async fn serve(
    session: &Session,
    inputs: &mut mpsc::Receiver<Bytes>,
    updates: &mpsc::Sender<Envelope>,
) -> Result<(), Fault> {
    let (launch, size) = (session.launch.clone(), session.size);
    let (terminal, mut events) = off_runtime(move || launch.open_login(size)).await?;
    send(updates, update(session, TerminalEvent::Opened)).await?;
    let (typed, to_write) = mpsc::channel(WAITING);
    // Dropped however the session ends, which ends the writes and the terminal they hold, as
    // a write the sign-in doesn't take waits for good.
    let mut writer = JoinSet::new();
    writer.spawn(write(terminal, to_write));
    loop {
        tokio::select! {
            event = events.next() => {
                let Some(Event::Output(data)) = event else {
                    return Ok(());
                };
                let data = Bytes::new(data);
                send(updates, update(session, TerminalEvent::Output { data })).await?;
            }
            input = inputs.recv() => {
                let Some(input) = input else {
                    return Ok(());
                };
                // An input that finds the queue full fails the terminal.
                typed.try_send(input).map_err(|_| input_failed())?;
            }
            Some(written) = writer.join_next() => {
                return written.map_err(|error| input_failed().because(error))?;
            }
        }
    }
}

// Inputs waiting for the terminal to take them.
const WAITING: usize = 64;

async fn write(terminal: Terminal, mut typed: mpsc::Receiver<Bytes>) -> Result<(), Fault> {
    while let Some(data) = typed.recv().await {
        let written = terminal.input(data.as_slice()).await;
        written.map_err(|error| input_failed().because(error))?;
    }
    Ok(())
}

/// `cols` by `rows`, if a terminal can be that size.
pub(super) fn size(cols: u32, rows: u32) -> Result<Size, Fault> {
    Size::new(cols, rows).ok_or_else(size_refused)
}

fn size_refused() -> Fault {
    Fault::new(
        code!("terminal.size_refused"),
        "The terminal's size was refused",
    )
}

/// Runs `work`, which touches files or starts processes, off the async runtime.
pub(super) async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
) -> Result<T, Fault> {
    crate::app::blocking(move || Ok(work()))
        .await
        .map_err(|error| {
            Fault::new(
                code!("terminal.start_failed"),
                "The terminal couldn't start",
            )
            .because(error)
        })?
}

fn input_failed() -> Fault {
    Fault::new(
        code!("terminal.input_failed"),
        "The terminal's input failed",
    )
}

fn update(session: &Session, event: TerminalEvent) -> TerminalUpdate {
    TerminalUpdate {
        terminal: session.terminal.clone(),
        workspace: session.workspace.clone(),
        event,
    }
}

/// Puts `update` on the uplink.
pub(super) async fn send(
    updates: &mpsc::Sender<Envelope>,
    update: TerminalUpdate,
) -> Result<(), Fault> {
    let message = Message::Terminal(update);
    updates
        .send(Envelope { id: 0, message })
        .await
        .map_err(|_| Fault::new(code!("terminal.gone"), "The terminal's connection is gone"))
}
