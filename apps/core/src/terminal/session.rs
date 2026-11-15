//! One sign-in terminal, from the core's handshake reply to its close.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use weacon_agents::{Event, Launch, SignIn, Size, Terminal};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    Bytes, Envelope, Message, SignInState, TerminalEvent, TerminalId, TerminalInput,
    TerminalOutput, TerminalUpdate, WorkspaceId, code,
};

use super::seal::{Channel, seal_failed};

/// A terminal whose handshake the core accepted, before its login starts.
pub(super) struct Session {
    pub(super) terminal: TerminalId,
    pub(super) workspace: WorkspaceId,
    pub(super) launch: Launch,
    pub(super) channel: Channel,
    pub(super) size: Size,
    pub(super) reply: Bytes,
}

// A browser that goes quiet, or a sign-in left open, doesn't hold its workspace's slot for good;
// an open page seals a frame more often than this, even while nothing is typed.
const QUIET: Duration = Duration::from_mins(1);
const SIGN_IN_LIMIT: Duration = Duration::from_mins(15);

/// Runs the terminal until its login ends, the browser closes it or the frames it sends stop,
/// anything fails or time runs out, then says it is closed. It holds `alive` until then.
pub(super) async fn run(
    mut session: Session,
    mut frames: mpsc::Receiver<Bytes>,
    alive: Arc<()>,
    updates: mpsc::Sender<Envelope>,
    log: Logger,
) {
    let served = timeout(
        SIGN_IN_LIMIT,
        serve(&mut session, &mut frames, &updates, &log),
    )
    .await;
    let served = served.unwrap_or_else(|_| Err(timed_out()));
    // Frees the workspace's slot before the browser hears the terminal is closed.
    drop((frames, alive));
    let code = served.err().map(|fault| {
        log.warn("terminal failed", &[("err", Field::error(&fault))]);
        fault.code().clone()
    });
    drop(send(&updates, update(&session, TerminalEvent::Closed { code })).await);
    log.info("terminal closed", &[]);
}

async fn serve(
    session: &mut Session,
    frames: &mut mpsc::Receiver<Bytes>,
    updates: &mpsc::Sender<Envelope>,
    log: &Logger,
) -> Result<(), Fault> {
    let accepted = TerminalEvent::Accepted {
        handshake: session.reply.clone(),
    };
    send(updates, update(session, accepted)).await?;
    // Only the browser that made the handshake can seal a frame, so a handshake played again
    // by anyone else starts nothing.
    let first = timeout(QUIET, frames.recv()).await;
    let Some(first) = first.map_err(|_| timed_out())? else {
        return Ok(());
    };
    let first = session.channel.open(first.as_slice())?;
    let (launch, size) = (session.launch.clone(), session.size);
    let (terminal, mut events) = off_runtime(move || launch.open_login(size)).await?;
    let (typed, to_write) = mpsc::channel(WAITING);
    // Dropped however the session ends, which ends the writes and the terminal they hold, as
    // a write the sign-in doesn't take waits for good.
    let mut writer = JoinSet::new();
    writer.spawn(write(terminal, to_write));
    pass(&typed, &first)?;
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            event = events.next() => {
                // A browser that is gone doesn't take what is shown, so showing it can't wait.
                let by = heard + QUIET;
                let Some(Event::Output(data)) = event else {
                    let sign_in = signed_in(&session.launch, log).await;
                    return shown(session, updates, &TerminalOutput::Ended { sign_in }, by).await;
                };
                let data = Bytes::new(data).ok_or_else(seal_failed)?;
                shown(session, updates, &TerminalOutput::Output { data }, by).await?;
            }
            sealed = frames.recv() => {
                let Some(sealed) = sealed else {
                    return Ok(());
                };
                heard = Instant::now();
                pass(&typed, &session.channel.open(sealed.as_slice())?)?;
            }
            () = sleep_until(heard + QUIET) => return Err(timed_out()),
            Some(written) = writer.join_next() => {
                return written.map_err(|error| input_failed().because(error))?;
            }
        }
    }
}

// Inputs waiting for the terminal to take them.
const WAITING: usize = 64;

// Queues one opened frame for the terminal; a frame that finds the queue full fails it.
fn pass(typed: &mpsc::Sender<TerminalInput>, plaintext: &[u8]) -> Result<(), Fault> {
    let input =
        TerminalInput::from_json(plaintext).map_err(|error| input_failed().because(error))?;
    typed.try_send(input).map_err(|_| input_failed())
}

async fn write(terminal: Terminal, mut typed: mpsc::Receiver<TerminalInput>) -> Result<(), Fault> {
    while let Some(input) = typed.recv().await {
        apply(&terminal, input).await?;
    }
    Ok(())
}

// Passes on one input; one this build doesn't know changes nothing.
async fn apply(terminal: &Terminal, input: TerminalInput) -> Result<(), Fault> {
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

// Sends `output` by the time `by`, or times the terminal out.
async fn shown(
    session: &mut Session,
    updates: &mpsc::Sender<Envelope>,
    output: &TerminalOutput,
    by: Instant,
) -> Result<(), Fault> {
    by_then(by, self::output(session, updates, output)).await
}

// What `work` gives, if it is done by `by`.
async fn by_then(by: Instant, work: impl Future<Output = Result<(), Fault>>) -> Result<(), Fault> {
    timeout_at(by, work).await.map_err(|_| timed_out())?
}

async fn output(
    session: &mut Session,
    updates: &mpsc::Sender<Envelope>,
    output: &TerminalOutput,
) -> Result<(), Fault> {
    let plaintext = output
        .to_json()
        .map_err(|error| seal_failed().because(error))?;
    let sealed = Bytes::new(session.channel.seal(&plaintext)?).ok_or_else(seal_failed)?;
    send(updates, update(session, TerminalEvent::Frame { sealed })).await
}

async fn signed_in(launch: &Launch, log: &Logger) -> SignInState {
    match launch.sign_in_status().await {
        Ok(SignIn::SignedIn) => SignInState::SignedIn,
        Ok(SignIn::SignedOut) => SignInState::SignedOut,
        Err(fault) => {
            log.warn("sign-in unknown", &[("err", Field::error(&fault))]);
            SignInState::Unknown
        }
    }
}

fn timed_out() -> Fault {
    Fault::new(
        code!("terminal.timed_out"),
        "The terminal was open too long",
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

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};

    use super::*;

    // Time is paused, so the wait ends as soon as nothing else can happen.
    #[tokio::test(start_paused = true)]
    async fn what_isnt_done_by_its_time_times_the_terminal_out() {
        let started = Instant::now();
        let by = started + QUIET;

        let done = by_then(by, ready(Ok(()))).await;
        let stuck = by_then(by, pending()).await;

        done.unwrap();
        assert_eq!(stuck.unwrap_err().code().as_str(), "terminal.timed_out");
        assert_eq!(started.elapsed(), QUIET);
    }
}
