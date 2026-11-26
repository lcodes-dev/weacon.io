//! The core's one connection to its Weacon: what it says, and what it does when asked.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_link::{Agent, CONNECT, FromCore, ToCore};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error, Message};

use crate::terminals::{self, Terminal};
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
    /// The chats whose turn is running, each with what stops it.
    pub chats: HashMap<String, Arc<Notify>>,
    /// The asks an agent is answering now.
    answering: HashSet<String>,
    /// The answers the server hasn't said it has.
    answers: HashMap<String, Result<String, String>>,
    /// The sign-ins open in a terminal here.
    pub terminals: HashMap<Agent, Terminal>,
    /// Told when the agents may have changed, as after a sign-in.
    pub changed: Arc<Notify>,
    /// Whether the agents are being looked at now, so that it is done one at a time.
    looking: bool,
    /// How many sign-ins were opened, which numbers them.
    pub opened: u64,
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
    terminals::close_all(&mut work);
    ended
}

/// Answers what the server asked under an id, once, however often the server repeats it.
fn answer(
    work: &Shared,
    id: String,
    answer: impl Future<Output = Result<String, String>> + Send + 'static,
) {
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
        let answer = answer.await;
        {
            let mut work = work.lock().unwrap();
            work.answering.remove(&id);
            work.answers.insert(id.clone(), answer.clone());
        }
        say(&work, FromCore::Answer { id, answer });
    });
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
        } => answer(work, id, async move {
            agents::ask(agent, &model, &system, &prompt).await
        }),
        ToCore::Push {
            id,
            chat,
            repository,
            to,
            message,
            name,
            mail,
        } => answer(work, id, async move {
            chats::push(&chat, repository, &to, &message, (&name, &mail)).await
        }),
        ToCore::Forget { id, chat } => answer(work, id, async move { chats::forget(&chat) }),
        ToCore::Taken { id } => {
            work.lock().unwrap().answers.remove(&id);
        }
        ToCore::TerminalOpen { agent, cols, rows } => {
            if terminals::open(work, agent, cols, rows).is_err() {
                say(work, FromCore::TerminalClose { agent });
            }
        }
        ToCore::TerminalData { agent, data } => terminals::typed(work, agent, &data),
        ToCore::TerminalSize { agent, cols, rows } => terminals::resize(work, agent, cols, rows),
        ToCore::TerminalClose { agent } => terminals::close(work, agent),
        ToCore::Turn(turn) => chats::turn(work, turn),
        ToCore::Stop { chat } => chats::stop(work, &chat),
        // Taken care of where the connection is held.
        ToCore::Replaced => {}
    }
}
