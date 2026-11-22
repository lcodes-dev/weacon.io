//! Chats: a person's conversation with an agent that works on a project, on one of that person's machines.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use core_link::{Agent, Event, Repository, ToCore};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, Chat, Turn, User};
use crate::teams::{membership, refused};
use crate::{ApiError, AppState, github, not_found};

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Chat>>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(db::chats(&state.pool, &team_id).await?))
}

#[derive(Deserialize)]
pub struct Start {
    agent: String,
    machine_id: String,
    branch: String,
    text: String,
}

/// Starts a chat with its first message, on one of the person's own machines.
pub async fn start(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(start): Json<Start>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let projects = db::projects(&state.pool, &team_id).await?;
    let project = projects.iter().find(|project| project.id == project_id);
    let project = project.ok_or_else(not_found)?;
    if !project
        .branches
        .iter()
        .any(|branch| branch.name == start.branch)
    {
        return Err(refused("A chat needs a branch the project maps"));
    }
    let machines = db::machines(&state.pool, &user.id).await?;
    if !machines
        .iter()
        .any(|machine| machine.id == start.machine_id)
    {
        return Err(refused("A chat runs on a machine of your own"));
    }
    let agent = Agent::named(&start.agent).ok_or_else(|| refused("Pick Claude Code or Codex"))?;
    let text = start.text.trim();
    if text.is_empty() {
        return Err(refused("Write what the agent should do"));
    }
    let id = db::start_chat(
        &state.pool,
        &project_id,
        &user.id,
        &start.machine_id,
        agent.program(),
        &start.branch,
        text,
    );
    let id = id.await?;
    run(&state, &id).await;
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
pub struct After {
    /// The last event the web app has, or nothing for all of them.
    #[serde(default)]
    after: String,
}

/// A chat and what happened in it after an event. Every member of the team may read it.
pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Query(After { after }): Query<After>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let chat = db::chat(&state.pool, &team_id, &id).await?;
    let chat = chat.ok_or_else(not_found)?;
    let events = db::chat_events(&state.pool, &id, &after).await?;
    let events: Vec<Value> = events
        .iter()
        .filter_map(|event| serde_json::from_str(event).ok())
        .collect();
    let connected = chat
        .machine_id
        .as_deref()
        .is_some_and(|machine| state.cores.connected(machine));
    let mut shown = json!(chat);
    shown["mine"] = json!(chat.user_id == user.id);
    shown["connected"] = json!(connected);
    shown["events"] = json!(events);
    Ok(Json(shown))
}

/// The chat, for the one person who may write in it.
async fn own(state: &AppState, user: &User, team_id: &str, id: &str) -> Result<Chat, ApiError> {
    membership(user, team_id)?;
    let chat = db::chat(&state.pool, team_id, id).await?;
    let chat = chat.ok_or_else(not_found)?;
    if chat.user_id != user.id {
        let message = "Only the person who started a chat writes in it";
        return Err(ApiError(StatusCode::FORBIDDEN, message.into()));
    }
    Ok(chat)
}

#[derive(Deserialize)]
pub struct Message {
    text: String,
}

/// The chat's next turn.
pub async fn say(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Json(message): Json<Message>,
) -> Result<StatusCode, ApiError> {
    let chat = own(&state, &user, &team_id, &id).await?;
    if chat.machine_id.is_none() {
        return Err(refused(
            "This chat's machine was removed, and its folder with it",
        ));
    }
    let text = message.text.trim();
    if text.is_empty() {
        return Err(refused("Write what the agent should do"));
    }
    if !db::ask_chat(&state.pool, &id, text).await? {
        let message = "The agent is still working. Stop it, or wait for it to finish";
        return Err(ApiError(StatusCode::CONFLICT, message.into()));
    }
    run(&state, &id).await;
    Ok(StatusCode::ACCEPTED)
}

/// Ends the chat's turn. Only the machine can, since the agent's program runs there.
pub async fn stop(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let chat = own(&state, &user, &team_id, &id).await?;
    let machine = chat.machine_id.filter(|_| chat.running);
    let stopped =
        machine.is_some_and(|machine| state.cores.send(&machine, ToCore::Stop { chat: id }));
    if !stopped && chat.running {
        return Err(refused(
            "The machine is away, so its agent can't be stopped from here",
        ));
    }
    Ok(StatusCode::ACCEPTED)
}

/// Tells a chat's machine to run its turn, if it is connected. One that is away is told when it connects.
async fn run(state: &AppState, id: &str) {
    if let Ok(Some(turn)) = db::running_turn(&state.pool, id).await {
        tell(state, turn).await;
    }
}

/// Tells a machine that just connected of the turns it should be running: those it never
/// heard of start, and those it was running say what the server missed.
pub async fn resume(state: &AppState, machine: &str) {
    let turns = db::running_turns(&state.pool, Some(machine)).await;
    for turn in turns.unwrap_or_default() {
        tell(state, turn).await;
    }
}

/// Ends the turns whose machine was removed, which nothing else would end.
pub async fn orphaned(state: &AppState) {
    let turns = db::running_turns(&state.pool, None).await;
    for turn in turns.unwrap_or_default() {
        let failed = db::fail_chat(&state.pool, &turn.id, "The machine was removed").await;
        failed.ok();
    }
}

async fn tell(state: &AppState, turn: Turn) {
    let (Some(machine), Some(repo), Some(agent)) =
        (&turn.machine_id, &turn.repo, Agent::named(&turn.agent))
    else {
        let failed = db::fail_chat(
            &state.pool,
            &turn.id,
            "The project has no repository any more",
        );
        return failed.await.unwrap_or_default();
    };
    if !state.cores.connected(machine) {
        return;
    }
    // The machine fetches with a token for this repository alone, which GitHub ends after an hour.
    let token = match github::token(state, &turn.team_id, Some(repo)).await {
        Ok(token) => token,
        Err(ApiError(_, why)) => {
            return db::fail_chat(&state.pool, &turn.id, &why)
                .await
                .unwrap_or_default();
        }
    };
    let repository = Repository {
        url: format!("https://github.com/{repo}.git"),
        branch: turn.branch,
        token,
    };
    let message = ToCore::Turn {
        chat: turn.id,
        agent,
        session: turn.session,
        prompt: turn.asked,
        repository,
        heard: turn.heard as u64,
    };
    state.cores.send(machine, message);
}

/// Takes in what a machine tells of a chat of its own.
pub async fn heard(state: &AppState, machine: &str, chat: &str, number: u64, event: Event) {
    let (running, session) = match &event {
        Event::Done { session, .. } => (false, Some(session.as_str())),
        Event::Failed { .. } => (false, None),
        Event::Said { .. } | Event::Did { .. } => (true, None),
    };
    let (number, body) = (number as i64, json!(event));
    let heard = db::hear_chat(&state.pool, machine, chat, number, &body, running, session);
    heard.await.ok();
}
