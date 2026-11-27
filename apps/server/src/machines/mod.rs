pub mod db;
pub mod http;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::http::HeaderMap;
use core_link::{Agent, Found, FromCore, Ready, ToCore};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::machines::db::Machine;
use crate::{AppState, auth, chats, live};

type Answer = std::result::Result<String, String>;

#[derive(Default)]
pub struct Cores(Mutex<Connected>);

#[derive(Default)]
struct Connected {
    cores: HashMap<String, (u64, mpsc::UnboundedSender<ToCore>)>,
    left: HashMap<String, Instant>,
    asks: HashMap<String, (String, ToCore, oneshot::Sender<Answer>)>,
    terminals: HashMap<(String, Agent), mpsc::UnboundedSender<FromCore>>,
    connections: u64,
}

impl Cores {
    fn lock(&self) -> MutexGuard<'_, Connected> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn connected(&self, machine: &str) -> bool {
        self.lock().cores.contains_key(machine)
    }

    pub fn send(&self, machine: &str, message: ToCore) -> bool {
        let cores = self.lock();
        let core = cores.cores.get(machine);
        core.is_some_and(|(_, core)| core.send(message).is_ok())
    }

    pub fn end(&self, machine: &str, why: &str) {
        let mut cores = self.lock();
        cores.cores.remove(machine);
        cores.terminals.retain(|(of, _), _| of != machine);
        let asked = cores.asks.extract_if(|_, (of, ..)| of == machine);
        for (_, (_, _, asker)) in asked {
            asker.send(Err(why.to_owned())).ok();
        }
    }

    pub fn open_terminal(&self, machine: &str, agent: Agent) -> Result<mpsc::UnboundedReceiver<FromCore>> {
        let mut cores = self.lock();
        if !cores.cores.contains_key(machine) {
            return Err(ApiError::invalid("This machine isn't connected"));
        }
        let key = (machine.to_owned(), agent);
        if cores.terminals.get(&key).is_some_and(|open| !open.is_closed()) {
            return Err(ApiError::invalid("This sign-in is already open in another window"));
        }
        let (to_browser, from_core) = mpsc::unbounded_channel();
        cores.terminals.insert(key, to_browser);
        Ok(from_core)
    }

    fn close_terminal(&self, machine: &str, agent: Agent) {
        self.lock().terminals.remove(&(machine.to_owned(), agent));
    }
}

const PING: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(75);
const SLOW: Duration = Duration::from_secs(20);

pub async fn serve(state: AppState, machine: Machine, mut socket: WebSocket) {
    let (machine, owner) = (machine.id, machine.user_id);
    let (connection, mut outgoing) = join(&state, &machine);
    tell_owner(&state, &owner);
    chats::resume_turns(&state, &machine).await;
    let mut alive = tokio::time::interval(PING);
    let mut last_heard = Instant::now();
    loop {
        let send = tokio::select! {
            message = outgoing.recv() => match message {
                Some(message) => Message::Text(json!(message).to_string().into()),
                None => break,
            },
            _ = alive.tick() => Message::Ping(Default::default()),
            _ = tokio::time::sleep_until(last_heard + SILENCE) => break,
            message = socket.recv() => {
                last_heard = Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(message) = serde_json::from_str(&text) {
                            heard(&state, &machine, &owner, message).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
                continue;
            }
        };
        let sent = tokio::time::timeout(SLOW, socket.send(send)).await;
        if !matches!(sent, Ok(Ok(()))) {
            break;
        }
    }
    leave(&state, &machine, connection);
    db::see(&state.pool, &machine, None).await.ok();
    tell_owner(&state, &owner);
}

fn join(state: &AppState, machine: &str) -> (u64, mpsc::UnboundedReceiver<ToCore>) {
    let (to_core, outgoing) = mpsc::unbounded_channel();
    let mut cores = state.cores.lock();
    cores.connections += 1;
    let connection = cores.connections;
    let newer = (connection, to_core.clone());
    if let Some((_, older)) = cores.cores.insert(machine.to_owned(), newer) {
        older.send(ToCore::Replaced).ok();
    }
    cores.left.remove(machine);
    let waiting = cores.asks.values().filter(|(asked, ..)| asked == machine);
    for (_, ask, _) in waiting {
        to_core.send(ask.clone()).ok();
    }
    (connection, outgoing)
}

fn leave(state: &AppState, machine: &str, connection: u64) {
    let mut cores = state.cores.lock();
    let current = cores.cores.get(machine);
    if current.is_some_and(|(open, _)| *open == connection) {
        cores.cores.remove(machine);
        cores.left.insert(machine.to_owned(), Instant::now());
        cores.terminals.retain(|(of, _), _| of != machine);
    }
}

fn tell_owner(state: &AppState, owner: &str) {
    live::user(state, owner, Topic::Machines);
    live::user(state, owner, Topic::Chats);
}

async fn heard(state: &AppState, machine: &str, owner: &str, message: FromCore) {
    match message {
        FromCore::Agents { agents } => {
            let agents = json!(agents).to_string();
            db::see(&state.pool, machine, Some(&agents)).await.ok();
            live::user(state, owner, Topic::Machines);
        }
        FromCore::Answer { id, answer } => {
            let mut cores = state.cores.lock();
            let asked = cores.asks.get(&id).is_some_and(|(of, ..)| of == machine);
            if asked && let Some((_, _, asker)) = cores.asks.remove(&id) {
                asker.send(answer).ok();
            }
            if let Some((_, core)) = cores.cores.get(machine) {
                core.send(ToCore::Taken { id }).ok();
            }
        }
        FromCore::Event { chat, number, event } => {
            chats::hear(state, machine, &chat, number, event).await;
        }
        FromCore::TerminalData { agent, .. } | FromCore::TerminalClose { agent } => {
            let cores = state.cores.lock();
            if let Some(browser) = cores.terminals.get(&(machine.to_owned(), agent)) {
                browser.send(message).ok();
            }
        }
    }
}

const PATIENCE: Duration = Duration::from_secs(15 * 60);
const AWAY: Duration = Duration::from_secs(10 * 60);

pub async fn ask(
    state: &AppState,
    user_id: &str,
    agent: Agent,
    model: &str,
    system: &str,
    prompt: &str,
) -> Result<String> {
    let machines = db::list(&state.pool, user_id).await?;
    let signed_in = Found { agent, ready: Ready::SignedIn };
    let ready = |machine: &&Machine| {
        let found: Vec<Found> = serde_json::from_str(&machine.agents).unwrap_or_default();
        state.cores.connected(&machine.id) && found.contains(&signed_in)
    };
    let Some(machine) = machines.iter().find(ready) else {
        let program = agent.program();
        return Err(ApiError::invalid(match machines.len() {
            0 => "Pair a machine of yours first, under Machines".to_owned(),
            _ => format!("None of your machines is connected with {program} signed in. Look under Machines"),
        }));
    };
    let slow = format!("{} took too long", agent.program());
    let message = |id| ToCore::Ask {
        id,
        agent,
        model: model.to_owned(),
        system: system.to_owned(),
        prompt: prompt.to_owned(),
    };
    answered(state, (&machine.id, &machine.name), &slow, message).await
}

pub async fn answered(
    state: &AppState,
    (machine, name): (&str, &str),
    slow: &str,
    message: impl FnOnce(String) -> ToCore,
) -> Result<String> {
    let id = uuid::Uuid::now_v7().to_string();
    let message = message(id.clone());
    let (asker, mut answer) = oneshot::channel();
    let asked = (machine.to_owned(), message.clone(), asker);
    state.cores.lock().asks.insert(id.clone(), asked);
    state.cores.send(machine, message);

    let started = Instant::now();
    let gave_up = loop {
        tokio::select! {
            answer = &mut answer => {
                let answer = answer.unwrap_or_else(|_| Err("Something went wrong".into()));
                return answer.map_err(ApiError::invalid);
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let left = state.cores.lock().left.get(machine).copied();
        if left.is_some_and(|left| left.elapsed() > AWAY) {
            break format!("{name} went away and didn't come back");
        }
        if started.elapsed() > PATIENCE {
            break slow.to_owned();
        }
    };
    state.cores.lock().asks.remove(&id);
    Err(ApiError::invalid(gave_up))
}

const TERMINAL_PING: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Typed {
    Data { data: String },
    Size { cols: u16, rows: u16 },
}

pub async fn relay(
    state: AppState,
    headers: HeaderMap,
    machine: String,
    agent: Agent,
    mut socket: WebSocket,
    mut from_core: mpsc::UnboundedReceiver<FromCore>,
) {
    let (cols, rows) = (80, 24);
    state.cores.send(&machine, ToCore::TerminalOpen { agent, cols, rows });
    let mut still = tokio::time::interval(TERMINAL_PING);
    loop {
        tokio::select! {
            _ = still.tick() => {
                if !matches!(auth::signed_in(&state, &headers).await, Ok(Some(_))) {
                    break;
                }
            }
            message = from_core.recv() => match message {
                Some(FromCore::TerminalData { data, .. }) => {
                    let shown = json!({"type": "data", "data": data}).to_string();
                    if socket.send(Message::Text(shown.into())).await.is_err() {
                        break;
                    }
                }
                _ => break,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    let message = match serde_json::from_str(&text) {
                        Ok(Typed::Data { data }) => ToCore::TerminalData { agent, data },
                        Ok(Typed::Size { cols, rows }) => ToCore::TerminalSize { agent, cols, rows },
                        Err(_) => continue,
                    };
                    state.cores.send(&machine, message);
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    state.cores.close_terminal(&machine, agent);
    state.cores.send(&machine, ToCore::TerminalClose { agent });
}
