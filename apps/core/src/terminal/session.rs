//! One sign-in terminal, from the core's handshake reply to its close.

use tokio::sync::mpsc;
use weacon_agents::{Event, Launch, SignIn, Size, Terminal};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    Bytes, SignInState, TerminalEvent, TerminalId, TerminalInput, TerminalOutput, TerminalUpdate,
    WorkspaceId, code,
};

use super::seal::{Seal, seal_failed};

/// A terminal whose handshake the core accepted, before its login starts.
pub(super) struct Session {
    pub(super) terminal: TerminalId,
    pub(super) workspace: WorkspaceId,
    pub(super) launch: Launch,
    pub(super) seal: Seal,
    pub(super) size: Size,
    pub(super) reply: Bytes,
}

/// What the browser sent a terminal.
pub(super) enum Input {
    Frame(Bytes),
    Close,
}

/// Runs the terminal until its login ends, the browser closes it or anything fails, then says
/// it is closed.
pub(super) async fn run(
    mut session: Session,
    mut inputs: mpsc::Receiver<Input>,
    updates: mpsc::Sender<TerminalUpdate>,
    log: Logger,
) {
    let served = serve(&mut session, &mut inputs, &updates).await;
    // Frees the workspace's slot before the browser hears the terminal is closed.
    drop(inputs);
    let code = served.err().map(|fault| {
        log.warn("terminal failed", &[("err", Field::error(&fault))]);
        fault.code().clone()
    });
    drop(
        updates
            .send(update(&session, TerminalEvent::Closed { code }))
            .await,
    );
    log.info("terminal closed", &[]);
}

async fn serve(
    session: &mut Session,
    inputs: &mut mpsc::Receiver<Input>,
    updates: &mpsc::Sender<TerminalUpdate>,
) -> Result<(), Fault> {
    let accepted = TerminalEvent::Accepted {
        handshake: session.reply.clone(),
    };
    send(updates, update(session, accepted)).await?;
    // Only the browser that made the handshake can seal a frame, so a handshake played again
    // by anyone else starts nothing.
    let Some(Input::Frame(first)) = inputs.recv().await else {
        return Ok(());
    };
    let first = session.seal.open(first.as_slice())?;
    let (terminal, mut events) = session.launch.log_in_terminal(session.size)?;
    apply(&terminal, &first).await?;
    loop {
        tokio::select! {
            event = events.next() => {
                let Some(Event::Output(data)) = event else {
                    let sign_in = signed_in(&session.launch).await;
                    return output(session, updates, &TerminalOutput::Ended { sign_in }).await;
                };
                let data = Bytes::new(data).ok_or_else(seal_failed)?;
                output(session, updates, &TerminalOutput::Output { data }).await?;
            }
            input = inputs.recv() => {
                let Some(Input::Frame(sealed)) = input else {
                    return Ok(());
                };
                apply(&terminal, &session.seal.open(sealed.as_slice())?).await?;
            }
        }
    }
}

// Passes on one opened frame; a frame this build doesn't know changes nothing.
async fn apply(terminal: &Terminal, plaintext: &[u8]) -> Result<(), Fault> {
    let input =
        TerminalInput::from_json(plaintext).map_err(|error| input_failed().because(error))?;
    let applied = match input {
        TerminalInput::Input { data } => terminal.input(data.as_slice()).await,
        TerminalInput::Resize { cols, rows } => terminal.resize(size(cols, rows)?),
        TerminalInput::Newer => Ok(()),
    };
    applied.map_err(|error| input_failed().because(error))
}

/// `cols` by `rows`, if a terminal can be that size.
pub(super) fn size(cols: u32, rows: u32) -> Result<Size, Fault> {
    Size::new(cols, rows).ok_or_else(size_refused)
}

pub(super) fn size_refused() -> Fault {
    Fault::new(
        code!("terminal.size_refused"),
        "The terminal's size was refused",
    )
}

fn input_failed() -> Fault {
    Fault::new(
        code!("terminal.input_failed"),
        "The terminal's input failed",
    )
}

async fn output(
    session: &mut Session,
    updates: &mpsc::Sender<TerminalUpdate>,
    output: &TerminalOutput,
) -> Result<(), Fault> {
    let plaintext = output
        .to_json()
        .map_err(|error| seal_failed().because(error))?;
    let sealed = Bytes::new(session.seal.seal(&plaintext)?).ok_or_else(seal_failed)?;
    send(updates, update(session, TerminalEvent::Frame { sealed })).await
}

async fn signed_in(launch: &Launch) -> SignInState {
    match launch.sign_in().await {
        Ok(SignIn::SignedIn) => SignInState::SignedIn,
        Ok(SignIn::SignedOut) => SignInState::SignedOut,
        Err(_) => SignInState::Unknown,
    }
}

fn update(session: &Session, event: TerminalEvent) -> TerminalUpdate {
    TerminalUpdate {
        terminal: session.terminal.clone(),
        workspace: session.workspace.clone(),
        event,
    }
}

async fn send(updates: &mpsc::Sender<TerminalUpdate>, update: TerminalUpdate) -> Result<(), Fault> {
    updates
        .send(update)
        .await
        .map_err(|_| Fault::new(code!("terminal.gone"), "The terminal's connection is gone"))
}
