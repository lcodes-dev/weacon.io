//! The core's one connection to its Weacon: what it says, and what it does when asked.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use core_link::{Agent, CONNECT, FromCore, ToCore};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error, Message};

use crate::{Paired, agents, home};

/// How often the core looks again at its agents.
const LOOK: Duration = Duration::from_secs(5 * 60);

/// What outlives a connection: the work asked for, and where to say things while connected.
#[derive(Default)]
struct Work {
    /// The way to the server, while there is one.
    out: Option<mpsc::UnboundedSender<FromCore>>,
    /// The asks an agent is answering now.
    answering: HashSet<String>,
    /// The answers the server hasn't said it has.
    answers: HashMap<String, Result<String, String>>,
    /// The sign-ins open in a terminal here.
    terminals: HashMap<Agent, Terminal>,
    /// Told when the agents may have changed, as after a sign-in.
    changed: Arc<Notify>,
}

struct Terminal {
    pty: Box<dyn MasterPty + Send>,
    typed: Box<dyn Write + Send>,
    sign_in: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

type Shared = Arc<Mutex<Work>>;

fn say(work: &Shared, message: FromCore) {
    if let Some(out) = &work.lock().unwrap().out {
        out.send(message).ok();
    }
}

async fn say_agents(work: &Shared) {
    let agents = agents::found().await;
    say(work, FromCore::Agents { agents });
}

/// Connects, and connects again whenever the connection is lost, for as long as the machine is paired.
pub async fn stay_connected(paired: Paired) {
    let work = Shared::default();
    let mut wait = Duration::from_secs(1);
    loop {
        match connected(&paired, &work).await {
            Ok(()) => wait = Duration::from_secs(1),
            Err(Error::Http(refused)) if refused.status() == StatusCode::UNAUTHORIZED => {
                // The machine was removed in Weacon.
                std::fs::remove_file(home().join("core.json")).ok();
                crate::fail("This machine was removed from Weacon. Pair it again to use it.");
            }
            Err(error) => eprintln!("Not connected to {}: {error}", paired.server),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

/// One connection, from its start to its loss.
async fn connected(paired: &Paired, work: &Shared) -> Result<(), Error> {
    let address = format!("{}{CONNECT}", paired.server.replacen("http", "ws", 1));
    let mut request = address.into_client_request()?;
    let bearer = format!("Bearer {}", paired.token)
        .parse()
        .expect("a token is plain text");
    request.headers_mut().insert("Authorization", bearer);
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    eprintln!("Connected to {}", paired.server);
    let (mut to_server, mut from_server) = socket.split();
    let (out, mut outgoing) = mpsc::unbounded_channel();
    {
        let mut work = work.lock().unwrap();
        // What was answered while away, or never taken, is said again.
        for (id, answer) in &work.answers {
            let (id, answer) = (id.clone(), answer.clone());
            out.send(FromCore::Answer { id, answer }).ok();
        }
        work.out = Some(out);
    }
    let changed = work.lock().unwrap().changed.clone();
    let mut look = tokio::time::interval(LOOK);
    let lost = loop {
        tokio::select! {
            // The first tick comes at once, so the server hears of the agents on connecting.
            _ = look.tick() => say_agents(work).await,
            _ = changed.notified() => say_agents(work).await,
            message = outgoing.recv() => {
                let text = serde_json::to_string(&message).expect("a message is JSON");
                if let Err(error) = to_server.send(Message::Text(text.into())).await {
                    break Err(error);
                }
            }
            message = from_server.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(message) = serde_json::from_str(&text) {
                        asked(work, message);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Err(error)) => break Err(error),
                Some(Ok(_)) => {}
            },
        }
    };
    let mut work = work.lock().unwrap();
    work.out = None;
    // A sign-in can't go on without whoever was looking at it.
    for (_, mut terminal) in work.terminals.drain() {
        terminal.sign_in.kill().ok();
    }
    lost
}

/// Does what the server asked.
fn asked(work: &Shared, message: ToCore) {
    match message {
        ToCore::Ask {
            id,
            agent,
            model,
            system,
            prompt,
        } => {
            let mut known = work.lock().unwrap();
            if let Some(answer) = known.answers.get(&id).cloned() {
                drop(known);
                return say(work, FromCore::Answer { id, answer });
            }
            // An ask the server repeats after a lost connection is already being answered.
            if !known.answering.insert(id.clone()) {
                return;
            }
            drop(known);
            let work = work.clone();
            tokio::spawn(async move {
                let answer = agents::ask(agent, &model, &system, &prompt).await;
                {
                    let mut work = work.lock().unwrap();
                    work.answering.remove(&id);
                    work.answers.insert(id.clone(), answer.clone());
                }
                say(&work, FromCore::Answer { id, answer });
            });
        }
        ToCore::Taken { id } => {
            work.lock().unwrap().answers.remove(&id);
        }
        ToCore::TerminalOpen { agent, cols, rows } => {
            if open(work, agent, cols, rows).is_err() {
                say(work, FromCore::TerminalClose { agent });
            }
        }
        ToCore::TerminalData { agent, data } => {
            let mut work = work.lock().unwrap();
            let typed = BASE64.decode(data).unwrap_or_default();
            if let Some(terminal) = work.terminals.get_mut(&agent) {
                terminal.typed.write_all(&typed).ok();
            }
        }
        ToCore::TerminalSize { agent, cols, rows } => {
            if let Some(terminal) = work.lock().unwrap().terminals.get(&agent) {
                terminal.pty.resize(size(cols, rows)).ok();
            }
        }
        ToCore::TerminalClose { agent } => {
            if let Some(mut terminal) = work.lock().unwrap().terminals.remove(&agent) {
                terminal.sign_in.kill().ok();
            }
        }
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Starts an agent's own sign-in in a terminal nobody sees here. Which command it is
/// comes from the agent alone: nothing the server sends can start anything else.
fn open(
    work: &Shared,
    agent: Agent,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = native_pty_system().openpty(size(cols, rows))?;
    let mut command = CommandBuilder::new(agent.program());
    command.args(agents::sign_in(agent));
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let mut shown = pair.master.try_clone_reader()?;
    let terminal = Terminal {
        typed: pair.master.take_writer()?,
        pty: pair.master,
        sign_in: child.clone_killer(),
    };
    if let Some(mut earlier) = work.lock().unwrap().terminals.insert(agent, terminal) {
        earlier.sign_in.kill().ok();
    }
    let work = work.clone();
    // A terminal is read as it comes, which only a thread of its own can wait for.
    std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(read @ 1..) = shown.read(&mut bytes) {
            let data = BASE64.encode(&bytes[..read]);
            say(&work, FromCore::TerminalData { agent, data });
        }
        child.wait().ok();
        work.lock().unwrap().terminals.remove(&agent);
        say(&work, FromCore::TerminalClose { agent });
        // Someone may be signed in now.
        work.lock().unwrap().changed.notify_one();
    });
    Ok(())
}
