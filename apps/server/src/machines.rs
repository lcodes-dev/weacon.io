//! A person's machines: pairing a core, its connection, and what is asked of it over that connection.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use core_link::{Agent, Found, FromCore, Ready, ToCore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::db::{self, Machine, User};
use crate::teams::refused;
use crate::{ApiError, AppState, not_found};

/// How long a pairing code works, in seconds.
const CODE_SECONDS: i64 = 600;
/// An agent can take minutes to answer.
const PATIENCE: Duration = Duration::from_secs(15 * 60);
/// How long an answer is waited for from a core that went away.
const AWAY: Duration = Duration::from_secs(10 * 60);
/// How often a connection is checked to be alive.
const PING: Duration = Duration::from_secs(30);

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

    fn connected(&self, machine: &str) -> bool {
        self.lock().cores.contains_key(machine)
    }

    /// Sends to a machine's core, and says whether it is connected.
    fn send(&self, machine: &str, message: ToCore) -> bool {
        let cores = self.lock();
        let core = cores.cores.get(machine);
        core.is_some_and(|(_, core)| core.send(message).is_ok())
    }

    /// Ends a machine's connection, when it has one.
    fn end(&self, machine: &str) {
        self.lock().cores.remove(machine);
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
    let user_id = match bearer(&headers) {
        Some(token) => db::machine_by_token(&state.pool, token)
            .await?
            .map(|machine| machine.user_id),
        None => crate::auth::signed_in(&state, &headers)
            .await?
            .map(|user| user.id),
    };
    let user_id =
        user_id.ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first".into()))?;
    let code = format!("{:032x}", rand::random::<u128>());
    db::add_machine_code(&state.pool, &code, &user_id).await?;
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
    state.cores.end(&id);
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
        // A core that connects twice keeps its newer connection: the older one ends when its sender goes.
        let connection = cores.connections;
        cores
            .cores
            .insert(machine.clone(), (connection, to_core.clone()));
        cores.left.remove(&machine);
        // What it was asked before it went away is asked again; a core knows an ask by its id.
        let waiting = cores.asks.values().filter(|(asked, ..)| *asked == machine);
        for (_, ask, _) in waiting {
            to_core.send(ask.clone()).ok();
        }
        connection
    };
    drop(to_core);
    let mut alive = tokio::time::interval(PING);
    loop {
        let sent = tokio::select! {
            message = outgoing.recv() => match message {
                Some(message) => socket.send(Message::Text(json!(message).to_string().into())).await,
                // The machine was removed, or its core connected anew.
                None => break,
            },
            _ = alive.tick() => socket.send(Message::Ping(Default::default())).await,
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(message) = serde_json::from_str(&text) {
                        heard(&state, &machine, message).await;
                    }
                    Ok(())
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => Ok(()),
            },
        };
        if sent.is_err() {
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
        }
        // A sign-in can't go on without its terminal.
        cores.terminals.retain(|(of, _), _| *of != machine);
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
            if let Some((_, _, asker)) = cores.asks.remove(&id) {
                asker.send(answer).ok();
            }
            // Also for an answer nobody waits for any more, so that the core forgets it.
            if let Some((_, core)) = cores.cores.get(machine) {
                core.send(ToCore::Taken { id }).ok();
            }
        }
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
    socket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
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
    Ok(socket.on_upgrade(move |socket| relay(state, id, agent, socket, from_core)))
}

async fn relay(
    state: AppState,
    machine: String,
    agent: Agent,
    mut socket: WebSocket,
    mut from_core: mpsc::UnboundedReceiver<FromCore>,
) {
    let (cols, rows) = (80, 24);
    state
        .cores
        .send(&machine, ToCore::TerminalOpen { agent, cols, rows });
    loop {
        tokio::select! {
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
