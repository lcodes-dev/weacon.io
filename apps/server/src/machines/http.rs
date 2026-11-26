use axum::Json;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use core_link::Agent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::db;
use super::db::Machine;
use crate::accounts::db::User;
use crate::auth::{bearer, random_token};
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::{AppState, auth, chats, live, now};

const CODE_SECONDS: i64 = 600;

#[derive(Serialize)]
pub struct Listed {
    #[serde(flatten)]
    machine: Machine,
    connected: bool,
}

pub async fn list_machines(State(state): State<AppState>, user: User) -> Result<Json<Vec<Listed>>> {
    let machines = db::list(&state.pool, &user.id).await?;
    let listed =
        machines.into_iter().map(|machine| Listed { connected: state.cores.connected(&machine.id), machine });
    Ok(Json(listed.collect()))
}

pub async fn code(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>> {
    let asker = match bearer(&headers) {
        Some(token) => {
            db::by_token(&state.pool, token).await?.map(|machine| (machine.user_id, Some(machine.id)))
        }
        None => auth::signed_in(&state, &headers).await?.map(|user| (user.id, None)),
    };
    let (user_id, machine_id) = asker.ok_or_else(|| ApiError::unauthorized("Sign in first"))?;
    let code = random_token();
    db::add_code(&state.pool, &code, &user_id, machine_id.as_deref()).await?;
    Ok(Json(json!({"code": code, "server": state.url})))
}

#[derive(Deserialize)]
pub struct Pairing {
    code: String,
    name: String,
    os: String,
}

pub async fn pair(State(state): State<AppState>, Json(pairing): Json<Pairing>) -> Result<Json<Value>> {
    let name = pairing.name.trim();
    if name.is_empty() || !["macos", "linux"].contains(&pairing.os.as_str()) {
        return Err(ApiError::invalid("A machine needs a name and runs macOS or Linux"));
    }
    let token = random_token();
    let since = now() - CODE_SECONDS;
    let paired = db::pair(&state.pool, &pairing.code, since, name, &pairing.os, &token);
    let Some(owner) = paired.await? else {
        let message = "This code doesn't work any more. Ask Weacon for a new one";
        return Err(ApiError::invalid(message));
    };
    live::user(&state, &owner, Topic::Machines);
    Ok(Json(json!({"token": token})))
}

pub async fn delete_machine(
    State(state): State<AppState>,
    user: User,
    Path(machine_id): Path<String>,
) -> Result<StatusCode> {
    if !db::delete(&state.pool, &user.id, &machine_id).await? {
        return Err(ApiError::not_found());
    }
    state.cores.end(&machine_id, "The machine was removed");
    chats::end_orphaned_turns(&state).await;
    live::user(&state, &user.id, Topic::Machines);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response> {
    let machine = match bearer(&headers) {
        Some(token) => db::by_token(&state.pool, token).await?,
        None => None,
    };
    let machine = machine.ok_or_else(|| ApiError::unauthorized("This machine isn't paired"))?;
    Ok(socket.on_upgrade(move |socket| super::serve(state, machine, socket)))
}

pub async fn sign_in(
    State(state): State<AppState>,
    user: User,
    Path((machine_id, agent)): Path<(String, String)>,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response> {
    auth::own_page(&state, &headers)?;
    let machines = db::list(&state.pool, &user.id).await?;
    let owned = machines.iter().any(|machine| machine.id == machine_id);
    let agent = Agent::named(&agent).filter(|_| owned);
    let agent = agent.ok_or_else(ApiError::not_found)?;
    let from_core = state.cores.open_terminal(&machine_id, agent)?;
    Ok(socket.on_upgrade(move |socket| super::relay(state, headers, machine_id, agent, socket, from_core)))
}
