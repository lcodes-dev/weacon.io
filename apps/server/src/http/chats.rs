use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use core_link::{Agent, ToCore};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::chats::{Chat, NewChat};
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, chats, cores, db, github};

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Chat>>> {
    user.member_of(&team_id)?;
    Ok(Json(
        db::chats::list(&state.pool, &team_id, &user.id, None).await?,
    ))
}

#[derive(Deserialize)]
pub struct Start {
    agent: String,
    machine_id: String,
    branch: String,
    text: String,
    /// The task of the project the chat is for, if any.
    task_id: Option<String>,
}

/// Starts a chat with its first message, on one of the person's own machines.
pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(start): Json<Start>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    let projects = db::projects::list(&state.pool, &team_id).await?;
    let project = projects.iter().find(|project| project.id == project_id);
    let project = project.ok_or_else(ApiError::not_found)?;
    if !project
        .branches
        .iter()
        .any(|branch| branch.name == start.branch)
    {
        return Err(ApiError::invalid("A chat needs a branch the project maps"));
    }
    let machines = db::machines::list(&state.pool, &user.id).await?;
    if !machines
        .iter()
        .any(|machine| machine.id == start.machine_id)
    {
        return Err(ApiError::invalid("A chat runs on a machine of your own"));
    }
    let agent = Agent::named(&start.agent);
    let agent = agent.ok_or_else(|| ApiError::invalid("Pick Claude Code or Codex"))?;
    if start.text.trim().is_empty() {
        return Err(ApiError::invalid("Write what the agent should do"));
    }
    if let Some(task) = &start.task_id {
        let tasks = db::tasks::list(&state.pool, &team_id, &project_id).await?;
        if !tasks.iter().any(|found| &found.id == task) {
            return Err(ApiError::invalid("A chat is for a task of its own project"));
        }
    }
    let chat = NewChat {
        project_id: &project_id,
        user_id: &user.id,
        machine_id: &start.machine_id,
        agent: agent.program(),
        branch: &start.branch,
        asked: &start.text,
        task_id: start.task_id.as_deref(),
        group: None,
    };
    let id = db::chats::start(&state.pool, &chat).await?;
    chats::start_turn(&state, &start.machine_id, &id).await?;
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
pub struct After {
    /// The last event or message the web app has, or nothing for all of them.
    #[serde(default)]
    pub after: String,
}

/// A chat and what happened in it after an event. Every member of the team may read it.
pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
    Query(After { after }): Query<After>,
) -> Result<Json<Value>> {
    let chat = readable(&state, &user, &team_id, &chat_id).await?;
    let events = db::chats::events(&state.pool, &chat_id, &after).await?;
    let events: Vec<Value> = events
        .iter()
        .filter_map(|event| serde_json::from_str(event).ok())
        .collect();
    let connected = chat.machine_id.as_deref();
    let connected = connected.is_some_and(|machine| state.cores.connected(machine));
    let mut shown = json!(chat);
    shown["mine"] = json!(chat.user_id == user.id);
    shown["connected"] = json!(connected);
    shown["events"] = json!(events);
    Ok(Json(shown))
}

#[derive(Deserialize)]
pub struct Message {
    text: String,
}

/// The chat's next turn.
pub async fn say(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
    Json(message): Json<Message>,
) -> Result<StatusCode> {
    let chat = own(&state, &user, &team_id, &chat_id).await?;
    if chat.group_id.is_some() {
        return Err(ApiError::invalid("This agent is called in its group"));
    }
    let machine = machine(&chat)?;
    let text = message.text.trim();
    if text.is_empty() {
        return Err(ApiError::invalid("Write what the agent should do"));
    }
    if !db::chats::ask(&state.pool, &chat_id, text, None).await? {
        return Err(busy());
    }
    chats::start_turn(&state, machine, &chat_id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// Ends the chat's turn. Only the machine can, since the agent's program runs there.
pub async fn stop(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let chat = own(&state, &user, &team_id, &chat_id).await?;
    if chat.running {
        let machine = connected(&state, &chat)?;
        state.cores.send(machine, ToCore::Stop { chat: chat_id });
    }
    Ok(StatusCode::ACCEPTED)
}

/// Deletes a chat, once its machine has removed its folder. A chat whose machine was removed
/// has none that can be reached.
pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let chat = own(&state, &user, &team_id, &chat_id).await?;
    if chat.running {
        return Err(busy());
    }
    if chat.machine_id.is_some() {
        let machine = connected(&state, &chat)?;
        let name = chat.machine.as_deref().unwrap_or_default();
        let slow = format!("{name} took too long to remove the chat's folder");
        let forget = |id| ToCore::Forget {
            id,
            chat: chat.id.clone(),
        };
        cores::answered(&state, (machine, name), &slow, forget).await?;
    }
    db::chats::delete(&state.pool, &chat.id).await?;
    live::team(&state, &team_id, Topic::Chats).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Host {
    host: String,
}

/// Lets the chat's commands reach a host that one of them was refused, and tells the agent
/// in a next turn to try again.
pub async fn allow_host(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
    Json(Host { host }): Json<Host>,
) -> Result<StatusCode> {
    let chat = own(&state, &user, &team_id, &chat_id).await?;
    let machine = machine(&chat)?;
    if !core_link::host(&host) {
        return Err(ApiError::invalid(
            "A host is an exact name, such as registry.npmjs.org",
        ));
    }
    db::chats::allow_host(&state.pool, &chat_id, &host).await?;
    let again = format!("{host} can be reached now. Try again what needed it.");
    if db::chats::ask(&state.pool, &chat_id, &again, None).await? {
        chats::start_turn(&state, machine, &chat_id).await?;
    }
    Ok(StatusCode::ACCEPTED)
}

/// Takes a host off those the chat's commands may reach, from its next turn on.
pub async fn forget_host(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id, host)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    own(&state, &user, &team_id, &chat_id).await?;
    db::chats::forget_host(&state.pool, &chat_id, &host).await?;
    live::chat(&state, &chat_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Everything a chat changed in its folder, as git writes it.
pub async fn diff(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    readable(&state, &user, &team_id, &chat_id).await?;
    let diff = db::chats::diff(&state.pool, &team_id, &chat_id).await?;
    Ok(Json(json!({"diff": diff.ok_or_else(ApiError::not_found)?})))
}

/// Has the chat's machine commit and push what its folder holds changed, and opens the
/// chat's pull request if it has none.
pub async fn push(
    State(state): State<AppState>,
    user: User,
    Path((team_id, chat_id)): Path<(String, String)>,
    Json(message): Json<Message>,
) -> Result<Json<Value>> {
    let chat = own(&state, &user, &team_id, &chat_id).await?;
    let message = message.text.trim();
    if message.is_empty() {
        return Err(ApiError::invalid("Write what the commit should be called"));
    }
    if chat.running {
        return Err(busy());
    }
    let machine = connected(&state, &chat)?;
    let name = chat.machine.as_deref().unwrap_or_default();
    let projects = db::projects::list(&state.pool, &team_id).await?;
    let project = projects
        .iter()
        .find(|project| project.id == chat.project_id);
    let repo = project.and_then(|project| project.repo.as_deref());
    let repo = repo.ok_or_else(|| ApiError::invalid("The project has no repository any more"))?;

    // Each token may do one thing in this one repository: the machine's pushes, the server's
    // opens the pull request.
    let token = |may| github::token(&state, &team_id, Some(repo), may);
    let pushing = token(json!({"contents": "write"})).await?;
    let opening = token(json!({"pull_requests": "write", "contents": "read"})).await?;
    let to = pushed_to(&chat);
    let push = |id| ToCore::Push {
        id,
        chat: chat.id.clone(),
        repository: chats::repository(repo, chat.branch.clone(), pushing),
        to: to.clone(),
        message: message.to_owned(),
        name: user.name.clone(),
        mail: user.email.clone(),
    };
    let slow = format!("{name} took too long to push");
    let commit = cores::answered(&state, (machine, name), &slow, push).await?;

    let mut pull = json!({"commit": commit, "number": chat.pull_number, "url": chat.pull_url});
    if chat.pull_url.is_none() {
        let title = message.lines().next().unwrap_or_default();
        let asker = format!(
            "Asked for by {} in a Weacon chat: {}/chats/{}",
            user.name, state.url, chat.id
        );
        let opened =
            github::open_pull(&state, &opening, repo, (&to, &chat.branch), (title, &asker));
        let opened = opened.await?;
        db::chats::set_pull(&state.pool, &chat.id, opened.number, &opened.html_url).await?;
        live::chat(&state, &chat.id).await;
        pull["number"] = json!(opened.number);
        pull["url"] = json!(opened.html_url);
    }
    Ok(Json(pull))
}

/// The chat, for a member of its team, or of its group if it has one.
async fn readable(state: &AppState, user: &User, team_id: &str, chat_id: &str) -> Result<Chat> {
    user.member_of(team_id)?;
    let chats = db::chats::list(&state.pool, team_id, &user.id, Some(chat_id)).await?;
    chats.into_iter().next().ok_or_else(ApiError::not_found)
}

/// The chat, for the one person who may write in it.
async fn own(state: &AppState, user: &User, team_id: &str, chat_id: &str) -> Result<Chat> {
    let chat = readable(state, user, team_id, chat_id).await?;
    if chat.user_id != user.id {
        return Err(ApiError::forbidden(
            "Only the person who started a chat writes in it",
        ));
    }
    Ok(chat)
}

/// The chat's machine, where its folder and its agent are.
fn machine(chat: &Chat) -> Result<&str> {
    let gone = || ApiError::invalid("This chat's machine was removed, and its folder with it");
    chat.machine_id.as_deref().ok_or_else(gone)
}

/// The chat's machine, for what can't wait until it is back.
fn connected<'a>(state: &AppState, chat: &'a Chat) -> Result<&'a str> {
    let machine = machine(chat)?;
    if !state.cores.connected(machine) {
        let name = chat.machine.as_deref().unwrap_or_default();
        let message = format!("{name} is away, and the chat's folder and agent are on it");
        return Err(ApiError::invalid(message));
    }
    Ok(machine)
}

/// Why nothing new can be asked of a chat whose turn is running.
fn busy() -> ApiError {
    ApiError::conflict("The agent is still working. Stop it, or wait for it to finish")
}

/// The branch a chat's work is pushed to: its title, and the end of its id to tell it from a
/// chat called the same.
fn pushed_to(chat: &Chat) -> String {
    let letters = chat.title.to_lowercase();
    let words = letters.split(|c: char| !c.is_ascii_alphanumeric());
    let words: Vec<&str> = words.filter(|word| !word.is_empty()).collect();
    let title: String = words.join("-").chars().take(40).collect();
    let end = &chat.id[chat.id.len() - 4..];
    format!("weacon/{}-{end}", title.trim_end_matches('-'))
}
