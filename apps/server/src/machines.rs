//! A person's machines: pairing a core, its connection, and what is asked of it over that connection.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header::{AUTHORIZATION, ORIGIN};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use core_link::{Agent, Found, FromCore, Ready, ToCore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::db::{self, Machine, User};
use crate::teams::refused;
use crate::{ApiError, AppState, chats, not_found};

/// How long a pairing code works, in seconds.
const CODE_SECONDS: i64 = 600;
/// An agent can take minutes to answer.
const PATIENCE: Duration = Duration::from_secs(15 * 60);
/// How long an answer is waited for from a core that went away.
const AWAY: Duration = Duration::from_secs(10 * 60);
/// How often a connection is checked to be alive.
const PING: Duration = Duration::from_secs(30);
/// A core answers every ping, so one this quiet is gone, whatever the network still says.
const SILENCE: Duration = Duration::from_secs(75);
/// How long sending one message to a core may take.
const SLOW: Duration = Duration::from_secs(20);

type Answer = Result<String, String>;

/// The cores connected now and what is being asked of them. It lives in memory only:
/// a core connects again after the server restarts, and a run can't outlive the server.
#[derive(Default)]
pub struct Cores(Mutex<Connected>);

#[derive(Default)]
struct Connected {
    /// The way to each connected machine's core, with a number that tells one connection from the next.
    cores: HashMap<String, (u64, mpsc::UnboundedSender<ToCore>)>,
    /// When a machine's core went away, until it is back.
    left: HashMap<String, Instant>,
    /// What was asked and not yet answered, by the ask's id, with its machine and where the answer goes.
    asks: HashMap<String, (String, ToCore, oneshot::Sender<Answer>)>,
    /// The sign-in terminals open in a browser.
    terminals: HashMap<(String, Agent), mpsc::UnboundedSender<FromCore>>,
    connections: u64,
}

impl Cores {
    fn lock(&self) -> std::sync::MutexGuard<'_, Connected> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn connected(&self, machine: &str) -> bool {
        self.lock().cores.contains_key(machine)
    }

    /// Sends to a machine's core, and says whether it is connected.
    pub fn send(&self, machine: &str, message: ToCore) -> bool {
        let cores = self.lock();
        let core = cores.cores.get(machine);
        core.is_some_and(|(_, core)| core.send(message).is_ok())
    }

    /// Ends everything a removed machine was part of: its connection, its sign-ins, and what
    /// was asked of it, which is answered with `why` so that nothing waits for it.
    fn end(&self, machine: &str, why: &str) {
        let mut cores = self.lock();
        cores.cores.remove(machine);
        cores.terminals.retain(|(of, _), _| of != machine);
        let asked = cores.asks.extract_if(|_, (of, ..)| of == machine);
        for (_, (_, _, asker)) in asked {
            asker.send(Err(why.to_owned())).ok();
        }
    }
}

#[derive(Serialize)]
pub struct Listed {
    #[serde(flatten)]
    machine: Machine,
    connected: bool,
}

pub async fn list(
    State(state): State<AppState>,
    user: User,
) -> Result<Json<Vec<Listed>>, ApiError> {
    let machines = db::machines(&state.pool, &user.id).await?;
    let listed = machines.into_iter().map(|machine| Listed {
        connected: state.cores.connected(&machine.id),
        machine,
    });
    Ok(Json(listed.collect()))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// A pairing code for the person signed in, or for the owner of the machine whose core asks.
pub async fn code(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    // A code a machine asked for goes with that machine.
    let asker = match bearer(&headers) {
        Some(token) => db::machine_by_token(&state.pool, token)
            .await?
            .map(|machine| (machine.user_id, Some(machine.id))),
        None => crate::auth::signed_in(&state, &headers)
            .await?
            .map(|user| (user.id, None)),
    };
    let (user_id, machine_id) =
        asker.ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first".into()))?;
    let code = format!("{:032x}", rand::random::<u128>());
    db::add_machine_code(&state.pool, &code, &user_id, machine_id.as_deref()).await?;
    // The address the core is given is the one people open, which the server was told at startup.
    Ok(Json(json!({"code": code, "server": state.url})))
}

#[derive(Deserialize)]
pub struct Pairing {
    code: String,
    name: String,
    os: String,
}

/// Trades a pairing code for the token a core connects with.
pub async fn pair(
    State(state): State<AppState>,
    Json(pairing): Json<Pairing>,
) -> Result<Json<Value>, ApiError> {
    let name = pairing.name.trim();
    if name.is_empty() || !["macos", "linux"].contains(&pairing.os.as_str()) {
        return Err(refused("A machine needs a name and runs macOS or Linux"));
    }
    let token = format!("{:032x}", rand::random::<u128>());
    let since = crate::now() - CODE_SECONDS;
    let paired = db::pair_machine(&state.pool, &pairing.code, since, name, &pairing.os, &token);
    if !paired.await? {
        return Err(refused(
            "This code doesn't work any more. Ask Weacon for a new one",
        ));
    }
    Ok(Json(json!({"token": token})))
}

pub async fn remove(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !db::delete_machine(&state.pool, &user.id, &id).await? {
        return Err(not_found());
    }
    // Its core is refused when it connects again, and forgets its token.
    state.cores.end(&id, "The machine was removed");
    chats::orphaned(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A core's WebSocket.
pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let machine = match bearer(&headers) {
        Some(token) => db::machine_by_token(&state.pool, token).await?,
        None => None,
    };
    let unknown = || ApiError(StatusCode::UNAUTHORIZED, "This machine isn't paired".into());
    let machine = machine.ok_or_else(unknown)?;
    Ok(socket.on_upgrade(move |socket| serve(state, machine.id, socket)))
}

async fn serve(state: AppState, machine: String, mut socket: WebSocket) {
    let (to_core, mut outgoing) = mpsc::unbounded_channel();
    let connection = {
        let mut cores = state.cores.lock();
        cores.connections += 1;
        // A core that connects twice keeps its newer connection. The older one is told, so that
        // it doesn't dial again and take the connection back, and ends when its sender goes.
        let connection = cores.connections;
        let newer = (connection, to_core.clone());
        if let Some((_, older)) = cores.cores.insert(machine.clone(), newer) {
            older.send(ToCore::Replaced).ok();
        }
        cores.left.remove(&machine);
        // What it was asked before it went away is asked again; a core knows an ask by its id.
        let waiting = cores.asks.values().filter(|(asked, ..)| *asked == machine);
        for (_, ask, _) in waiting {
            to_core.send(ask.clone()).ok();
        }
        connection
    };
    drop(to_core);
    chats::resume(&state, &machine).await;
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
                            heard(&state, &machine, message).await;
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
    {
        let mut cores = state.cores.lock();
        if cores
            .cores
            .get(&machine)
            .is_some_and(|(open, _)| *open == connection)
        {
            cores.cores.remove(&machine);
            cores.left.insert(machine.clone(), Instant::now());
            // A sign-in can't go on without its terminal. One opened over a newer connection stays.
            cores.terminals.retain(|(of, _), _| *of != machine);
        }
    }
    db::see_machine(&state.pool, &machine, None).await.ok();
}

/// Takes in what a core said.
async fn heard(state: &AppState, machine: &str, message: FromCore) {
    match message {
        FromCore::Agents { agents } => {
            let agents = json!(agents).to_string();
            db::see_machine(&state.pool, machine, Some(&agents))
                .await
                .ok();
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
        } => chats::heard(state, machine, &chat, number, event).await,
        FromCore::TerminalData { agent, .. } | FromCore::TerminalClose { agent } => {
            let cores = state.cores.lock();
            if let Some(browser) = cores.terminals.get(&(machine.to_owned(), agent)) {
                browser.send(message).ok();
            }
        }
    }
}

/// Has an agent on one of a person's machines answer a prompt, on that person's own sign-in.
pub async fn ask(
    state: &AppState,
    user_id: &str,
    agent: Agent,
    model: &str,
    system: &str,
    prompt: &str,
) -> Answer {
    let machines = db::machines(&state.pool, user_id).await;
    let machines = machines.map_err(|_| "Something went wrong")?;
    let ready = |machine: &&Machine| {
        let signed_in = Found {
            agent,
            ready: Ready::SignedIn,
        };
        let found: Vec<Found> = serde_json::from_str(&machine.agents).unwrap_or_default();
        state.cores.connected(&machine.id) && found.contains(&signed_in)
    };
    let Some(machine) = machines.iter().find(ready) else {
        let program = agent.program();
        return Err(match machines.len() {
            0 => "Pair a machine of yours first, under Machines".to_owned(),
            _ => format!(
                "None of your machines is connected with {program} signed in. Look under Machines"
            ),
        });
    };
    let id = uuid::Uuid::now_v7().to_string();
    let message = ToCore::Ask {
        id: id.clone(),
        agent,
        model: model.to_owned(),
        system: system.to_owned(),
        prompt: prompt.to_owned(),
    };
    let (asker, mut answer) = oneshot::channel();
    let asked = (machine.id.clone(), message.clone(), asker);
    state.cores.lock().asks.insert(id.clone(), asked);
    state.cores.send(&machine.id, message);

    let started = Instant::now();
    let gave_up = loop {
        tokio::select! {
            answer = &mut answer => return answer.unwrap_or_else(|_| Err("Something went wrong".into())),
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let left = state.cores.lock().left.get(&machine.id).copied();
        if left.is_some_and(|left| left.elapsed() > AWAY) {
            break format!("{} went away and didn't come back", machine.name);
        }
        if started.elapsed() > PATIENCE {
            break format!("{} took too long", agent.program());
        }
    };
    state.cores.lock().asks.remove(&id);
    Err(gave_up)
}

/// What a browser sends to a sign-in's terminal.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Typed {
    Data { data: String },
    Size { cols: u16, rows: u16 },
}

/// A browser's WebSocket to the terminal in which an agent's own sign-in runs on a machine.
/// What passes is handed on and kept nowhere.
pub async fn sign_in(
    State(state): State<AppState>,
    user: User,
    Path((id, agent)): Path<(String, String)>,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // A browser sends the cookie along from any page of the same site, so the page itself
    // must be Weacon's own.
    if headers
        .get(ORIGIN)
        .is_none_or(|origin| origin.as_bytes() != state.url.as_bytes())
    {
        let message = "A sign-in is opened from Weacon's own pages";
        return Err(ApiError(StatusCode::FORBIDDEN, message.into()));
    }
    let machines = db::machines(&state.pool, &user.id).await?;
    let owned = machines.iter().any(|machine| machine.id == id);
    let agent = Agent::named(&agent)
        .filter(|_| owned)
        .ok_or_else(not_found)?;
    let (to_browser, from_core) = mpsc::unbounded_channel();
    {
        let mut cores = state.cores.lock();
        let key = (id.clone(), agent);
        if !cores.cores.contains_key(&id) {
            return Err(refused("This machine isn't connected"));
        }
        if cores
            .terminals
            .get(&key)
            .is_some_and(|open| !open.is_closed())
        {
            return Err(refused("This sign-in is already open in another window"));
        }
        cores.terminals.insert(key, to_browser);
    }
    Ok(socket.on_upgrade(move |socket| relay(state, headers, id, agent, socket, from_core)))
}

async fn relay(
    state: AppState,
    headers: HeaderMap,
    machine: String,
    agent: Agent,
    mut socket: WebSocket,
    mut from_core: mpsc::UnboundedReceiver<FromCore>,
) {
    let (cols, rows) = (80, 24);
    state
        .cores
        .send(&machine, ToCore::TerminalOpen { agent, cols, rows });
    let mut still = tokio::time::interval(PING);
    loop {
        tokio::select! {
            // The terminal ends with the session it was opened in.
            _ = still.tick() => {
                if !matches!(crate::auth::signed_in(&state, &headers).await, Ok(Some(_))) {
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
                // The sign-in ended, or the core went away.
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
    state
        .cores
        .lock()
        .terminals
        .remove(&(machine.clone(), agent));
    state.cores.send(&machine, ToCore::TerminalClose { agent });
}
