use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use core_link::{FromCore, ToCore};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::db::machines::Machine;
use crate::live::{self, Topic};
use crate::{AppState, chats, db};

/// How often a connection is checked to be alive.
const PING: Duration = Duration::from_secs(30);
/// A core answers every ping, so one this quiet is gone, whatever the network still says.
const SILENCE: Duration = Duration::from_secs(75);
/// How long sending one message to a core may take.
const SLOW: Duration = Duration::from_secs(20);

/// A core's connection, for as long as it lasts.
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
                // The machine was removed, or its core connected anew.
                None => break,
            },
            _ = alive.tick() => Message::Ping(Default::default()),
            // A network that dropped without a word never says so itself.
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
                    // A pong, among others.
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
    db::machines::see(&state.pool, &machine, None).await.ok();
    tell_owner(&state, &owner);
}

/// Takes the machine's new connection, with a number that tells it from the next, and what
/// is to be sent over it.
fn join(state: &AppState, machine: &str) -> (u64, mpsc::UnboundedReceiver<ToCore>) {
    let (to_core, outgoing) = mpsc::unbounded_channel();
    let mut cores = state.cores.lock();
    cores.connections += 1;
    let connection = cores.connections;
    // A core that connects twice keeps its newer connection. The older one is told, so that it
    // doesn't dial again and take the connection back, and ends when its sender goes.
    let newer = (connection, to_core.clone());
    if let Some((_, older)) = cores.cores.insert(machine.to_owned(), newer) {
        older.send(ToCore::Replaced).ok();
    }
    cores.left.remove(machine);
    // What it was asked before it went away is asked again; a core knows an ask by its id.
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
        // A sign-in can't go on without its terminal. One opened over a newer connection stays.
        cores.terminals.retain(|(of, _), _| of != machine);
    }
}

/// Tells a machine's owner that it came or went, which their chats on it show too.
fn tell_owner(state: &AppState, owner: &str) {
    live::user(state, owner, Topic::Machines);
    live::user(state, owner, Topic::Chats);
}

/// Takes in what a core said.
async fn heard(state: &AppState, machine: &str, owner: &str, message: FromCore) {
    match message {
        FromCore::Agents { agents } => {
            let agents = json!(agents).to_string();
            db::machines::see(&state.pool, machine, Some(&agents))
                .await
                .ok();
            live::user(state, owner, Topic::Machines);
        }
        FromCore::Answer { id, answer } => {
            let mut cores = state.cores.lock();
            // Only the machine that was asked may answer.
            let asked = cores.asks.get(&id).is_some_and(|(of, ..)| of == machine);
            if asked && let Some((_, _, asker)) = cores.asks.remove(&id) {
                asker.send(answer).ok();
            }
            // Also for an answer nobody waits for any more, so that the core forgets it.
            if let Some((_, core)) = cores.cores.get(machine) {
                core.send(ToCore::Taken { id }).ok();
            }
        }
        FromCore::Event {
            chat,
            number,
            event,
        } => {
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
