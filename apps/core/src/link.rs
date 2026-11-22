//! The core's one connection to its Weacon: what it says, and what it does when asked.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use core_link::{Agent, CONNECT, FromCore, ToCore};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error, Message};

use crate::{Paired, agents, chats, fail, home};

/// How often the core looks again at its agents.
const LOOK: Duration = Duration::from_secs(5 * 60);
/// The server pings every 30 seconds, so a connection this quiet is dead, as after a laptop slept.
const SILENCE: Duration = Duration::from_secs(75);
/// How long connecting, or sending one message, may take.
const SLOW: Duration = Duration::from_secs(20);

/// What outlives a connection: the work asked for, and where to say things while connected.
#[derive(Default)]
pub struct Work {
    /// The way to the server, while there is one.
    pub out: Option<mpsc::UnboundedSender<FromCore>>,
    /// The chats that have had a turn since the core started.
    pub chats: HashMap<String, chats::Chat>,
    /// The asks an agent is answering now.
    answering: HashSet<String>,
    /// The answers the server hasn't said it has.
    answers: HashMap<String, Result<String, String>>,
    /// The sign-ins open in a terminal here.
    terminals: HashMap<Agent, Terminal>,
    /// Told when the agents may have changed, as after a sign-in.
    changed: Arc<Notify>,
    /// Whether the agents are being looked at now, so that it is done one at a time.
    looking: bool,
    /// How many sign-ins were opened, which numbers them.
    opened: u64,
}

struct Terminal {
    /// Tells this sign-in from one opened for the same agent after it.
    number: u64,
    pty: Box<dyn MasterPty + Send>,
    /// To the thread that writes what is typed, so that a terminal that doesn't read holds nothing else up.
    typed: std::sync::mpsc::Sender<Vec<u8>>,
    sign_in: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

/// Why a connection ended.
enum Ended {
    /// It was lost, or never made: dial again.
    Lost(String),
    /// The machine was removed in Weacon.
    Refused,
    /// Another core connected as this machine.
    Replaced,
}

pub type Shared = Arc<Mutex<Work>>;

fn say(work: &Shared, message: FromCore) {
    if let Some(out) = &work.lock().unwrap().out {
        out.send(message).ok();
    }
}

/// Looks at the agents on the side and tells the server, unless that is being done already.
fn look_at_agents(work: &Shared) {
    if std::mem::replace(&mut work.lock().unwrap().looking, true) {
        return;
    }
    let work = work.clone();
    tokio::spawn(async move {
        let agents = agents::found().await;
        work.lock().unwrap().looking = false;
        say(&work, FromCore::Agents { agents });
    });
}

/// Connects, and connects again whenever the connection is lost, for as long as the machine is paired.
pub async fn stay_connected(paired: Paired) {
    // Two cores on one pairing would take the connection from each other, and answer twice.
    let lock = std::fs::File::create(home().join("core.lock"));
    let lock =
        lock.unwrap_or_else(|error| fail(format!("The core's folder can't be written: {error}")));
    if lock.try_lock().is_err() {
        fail("weacon-core is already running for this pairing, perhaps in the background.");
    }
    let work = Shared::default();
    let mut wait = Duration::from_secs(1);
    loop {
        match connected(&paired, &work).await {
            Ended::Lost(why) => eprintln!("Not connected to {}: {why}", paired.server),
            Ended::Refused => {
                // Only the token this core started with is forgotten: the machine may have been paired anew.
                if crate::paired().is_some_and(|now| now.token == paired.token) {
                    std::fs::remove_file(home().join("core.json")).ok();
                }
                fail("This machine was removed from Weacon. Pair it again to use it.");
            }
            Ended::Replaced => {
                fail("Another weacon-core connected as this machine, so this one stops.")
            }
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

/// One connection, from its start to its end.
async fn connected(paired: &Paired, work: &Shared) -> Ended {
    let address = format!("{}{CONNECT}", paired.server.replacen("http", "ws", 1));
    let lost = |error: &dyn std::fmt::Display| Ended::Lost(error.to_string());
    let mut request = match address.into_client_request() {
        Ok(request) => request,
        Err(error) => return lost(&error),
    };
    let bearer = format!("Bearer {}", paired.token);
    let bearer = bearer.parse().expect("a token is plain text");
    request.headers_mut().insert("Authorization", bearer);
    let dialled = tokio::time::timeout(SLOW, tokio_tungstenite::connect_async(request)).await;
    let socket = match dialled {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(Error::Http(refused))) if refused.status() == StatusCode::UNAUTHORIZED => {
            return Ended::Refused;
        }
        Ok(Err(error)) => return lost(&error),
        Err(_) => return lost(&"it didn't answer"),
    };
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
    let mut heard = Instant::now();
    let ended = loop {
        tokio::select! {
            // The first tick comes at once, so the server hears of the agents on connecting.
            _ = look.tick() => look_at_agents(work),
            _ = changed.notified() => look_at_agents(work),
            _ = tokio::time::sleep_until(heard + SILENCE) => break lost(&"the server went quiet"),
            message = outgoing.recv() => {
                let text = serde_json::to_string(&message).expect("a message is JSON");
                let sent = to_server.send(Message::Text(text.into()));
                match tokio::time::timeout(SLOW, sent).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => break lost(&error),
                    Err(_) => break lost(&"the server stopped taking messages"),
                }
            }
            message = from_server.next() => {
                heard = Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                        Ok(ToCore::Replaced) => break Ended::Replaced,
                        Ok(message) => asked(work, message),
                        Err(_) => {}
                    },
                    Some(Ok(Message::Close(_))) | None => break lost(&"the server closed the connection"),
                    Some(Err(error)) => break lost(&error),
                    // A ping, which is answered by itself.
                    Some(Ok(_)) => {}
                }
            }
        }
    };
    let mut work = work.lock().unwrap();
    work.out = None;
    // A sign-in can't go on without whoever was looking at it.
    for (_, mut terminal) in work.terminals.drain() {
        terminal.sign_in.kill().ok();
    }
    ended
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
            let typed = BASE64.decode(data).unwrap_or_default();
            if let Some(terminal) = work.lock().unwrap().terminals.get(&agent) {
                terminal.typed.send(typed).ok();
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
        ToCore::Turn {
            chat,
            agent,
            session,
            prompt,
            repository,
            heard,
        } => chats::turn(work, chat, agent, session, prompt, repository, heard),
        ToCore::Stop { chat } => chats::stop(work, &chat),
        // Taken care of where the connection is held.
        ToCore::Replaced => {}
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
    let mut writer = pair.master.take_writer()?;
    let (typed, typing) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        // Ends when the terminal is forgotten, which drops the sender.
        for typed in typing {
            if writer.write_all(&typed).is_err() {
                break;
            }
        }
    });
    let number = {
        let mut work = work.lock().unwrap();
        work.opened += 1;
        let terminal = Terminal {
            number: work.opened,
            typed,
            pty: pair.master,
            sign_in: child.clone_killer(),
        };
        if let Some(mut earlier) = work.terminals.insert(agent, terminal) {
            earlier.sign_in.kill().ok();
        }
        work.opened
    };
    let work = work.clone();
    // A terminal is read as it comes, which only a thread of its own can wait for.
    std::thread::spawn(move || {
        // A sign-in that was closed or replaced says nothing more, so that it can't speak for its successor.
        let current = |work: &Work| {
            work.terminals
                .get(&agent)
                .is_some_and(|open| open.number == number)
        };
        let mut bytes = [0; 4096];
        while let Ok(read @ 1..) = shown.read(&mut bytes) {
            let data = BASE64.encode(&bytes[..read]);
            let work = work.lock().unwrap();
            if let (true, Some(out)) = (current(&work), &work.out) {
                out.send(FromCore::TerminalData { agent, data }).ok();
            }
        }
        child.wait().ok();
        let mut work = work.lock().unwrap();
        if current(&work) {
            work.terminals.remove(&agent);
            if let Some(out) = &work.out {
                out.send(FromCore::TerminalClose { agent }).ok();
            }
        }
        // Someone may be signed in now.
        work.changed.notify_one();
    });
    Ok(())
}
