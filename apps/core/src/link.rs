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

const LOOK: Duration = Duration::from_secs(5 * 60);
const SILENCE: Duration = Duration::from_secs(75);
const SLOW: Duration = Duration::from_secs(20);

#[derive(Default)]
pub struct Work {
    pub out: Option<mpsc::UnboundedSender<FromCore>>,
    pub chats: HashMap<String, Arc<Notify>>,
    answering: HashSet<String>,
    answers: HashMap<String, Result<String, String>>,
    pub terminals: HashMap<Agent, Terminal>,
    pub changed: Arc<Notify>,
    looking: bool,
    pub opened: u64,
}

enum Ended {
    Lost(String),
    Refused,
    Replaced,
}

pub type Shared = Arc<Mutex<Work>>;

fn say(work: &Shared, message: FromCore) {
    if let Some(out) = &work.lock().unwrap().out {
        out.send(message).ok();
    }
}

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

pub async fn stay_connected(paired: Paired) {
    let lock = std::fs::File::create(home().join("core.lock"));
    let lock = lock.unwrap_or_else(|error| fail(format!("The core's folder can't be written: {error}")));
    if lock.try_lock().is_err() {
        fail("weacon-core is already running for this pairing, perhaps in the background.");
    }
    let work = Shared::default();
    let mut wait = Duration::from_secs(1);
    loop {
        match connected(&paired, &work).await {
            Ended::Lost(why) => eprintln!("Not connected to {}: {why}", paired.server),
            Ended::Refused => {
                if crate::paired().is_some_and(|now| now.token == paired.token) {
                    std::fs::remove_file(home().join("core.json")).ok();
                }
                fail("This machine was removed from Weacon. Pair it again to use it.");
            }
            Ended::Replaced => fail("Another weacon-core connected as this machine, so this one stops."),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

async fn connected(paired: &Paired, work: &Shared) -> Ended {
    let address = format!("{}{CONNECT}", paired.server.replacen("http", "ws", 1));
    let lost = |error: &dyn std::fmt::Display| Ended::Lost(error.to_string());
    let mut request = match address.into_client_request() {
        Ok(request) => request,
        Err(error) => return lost(&error),
    };
    let bearer = format!("Bearer {}", paired.token).parse().expect("a token is plain text");
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

fn answer(work: &Shared, id: String, answer: impl Future<Output = Result<String, String>> + Send + 'static) {
    let mut known = work.lock().unwrap();
    if let Some(answer) = known.answers.get(&id).cloned() {
        drop(known);
        return say(work, FromCore::Answer { id, answer });
    }
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

fn asked(work: &Shared, message: ToCore) {
    match message {
        ToCore::Ask { id, agent, model, system, prompt } => {
            answer(work, id, async move { agents::ask(agent, &model, &system, &prompt).await })
        }
        ToCore::Push { id, chat, repository, to, message, name, mail } => {
            answer(
                work,
                id,
                async move { chats::push(&chat, repository, &to, &message, (&name, &mail)).await },
            )
        }
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
        ToCore::Replaced => {}
    }
}
