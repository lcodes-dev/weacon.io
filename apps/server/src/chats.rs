//! Chats: a person's conversation with an agent that works on a project, on one of that person's machines.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use core_link::{Agent, Event, Repository, ToCore};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, Chat, Running, User};
use crate::teams::{membership, refused};
use crate::{ApiError, AppState, github, not_found};

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Chat>>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(db::chats(&state.pool, &team_id, None).await?))
}

#[derive(Deserialize)]
pub struct Start {
    agent: String,
    pub machine_id: String,
    pub branch: String,
    pub text: String,
    /// The task of the project the chat is for, if any.
    pub task_id: Option<String>,
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
    if start.text.trim().is_empty() {
        return Err(refused("Write what the agent should do"));
    }
    if let Some(task) = &start.task_id {
        let tasks = db::tasks(&state.pool, &team_id, &project_id).await?;
        if !tasks.iter().any(|found| &found.id == task) {
            return Err(refused("A chat is for a task of its own project"));
        }
    }
    let id = db::start_chat(&state.pool, &project_id, &user.id, agent.program(), &start).await?;
    run(&state, &start.machine_id, &id);
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
    let chat = read(&state, &user, &team_id, &id).await?;
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

/// The chat, for a member of its team.
async fn read(state: &AppState, user: &User, team_id: &str, id: &str) -> Result<Chat, ApiError> {
    membership(user, team_id)?;
    let chat = db::chats(&state.pool, team_id, Some(id)).await?.pop();
    chat.ok_or_else(not_found)
}

/// The chat, for the one person who may write in it.
async fn own(state: &AppState, user: &User, team_id: &str, id: &str) -> Result<Chat, ApiError> {
    let chat = read(state, user, team_id, id).await?;
    if chat.user_id != user.id {
        let message = "Only the person who started a chat writes in it";
        return Err(ApiError(StatusCode::FORBIDDEN, message.into()));
    }
    Ok(chat)
}

/// The chat's machine, where its folder and its agent are.
fn machine(chat: &Chat) -> Result<&str, ApiError> {
    let gone = || refused("This chat's machine was removed, and its folder with it");
    chat.machine_id.as_deref().ok_or_else(gone)
}

/// The chat's machine, for what can't wait until it is back.
fn connected<'a>(state: &AppState, chat: &'a Chat) -> Result<&'a str, ApiError> {
    let machine = machine(chat)?;
    if !state.cores.connected(machine) {
        let name = chat.machine.as_deref().unwrap_or_default();
        return Err(refused(format!(
            "{name} is away, and the chat's folder and agent are on it"
        )));
    }
    Ok(machine)
}

/// Why nothing new can be asked of a chat whose turn is running.
fn busy() -> ApiError {
    let message = "The agent is still working. Stop it, or wait for it to finish";
    ApiError(StatusCode::CONFLICT, message.into())
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
    let machine = machine(&chat)?;
    let text = message.text.trim();
    if text.is_empty() {
        return Err(refused("Write what the agent should do"));
    }
    if !db::ask_chat(&state.pool, &id, text).await? {
        return Err(busy());
    }
    run(&state, machine, &id);
    Ok(StatusCode::ACCEPTED)
}

/// Ends the chat's turn. Only the machine can, since the agent's program runs there.
pub async fn stop(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let chat = own(&state, &user, &team_id, &id).await?;
    if chat.running {
        let machine = connected(&state, &chat)?;
        state.cores.send(machine, ToCore::Stop { chat: id });
    }
    Ok(StatusCode::ACCEPTED)
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
    Path((team_id, id)): Path<(String, String)>,
    Json(Host { host }): Json<Host>,
) -> Result<StatusCode, ApiError> {
    let chat = own(&state, &user, &team_id, &id).await?;
    let machine = machine(&chat)?;
    if !core_link::host(&host) {
        return Err(refused(
            "A host is an exact name, such as registry.npmjs.org",
        ));
    }
    db::allow_chat_host(&state.pool, &id, &host).await?;
    let again = format!("{host} can be reached now. Try again what needed it.");
    if db::ask_chat(&state.pool, &id, &again).await? {
        run(&state, machine, &id);
    }
    Ok(StatusCode::ACCEPTED)
}

/// Takes a host off those the chat's commands may reach, from its next turn on.
pub async fn forget_host(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id, host)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    own(&state, &user, &team_id, &id).await?;
    db::forget_chat_host(&state.pool, &id, &host).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Tells a chat's machine to run the turn that was just asked for. A machine that is away
/// is told when it connects. Done on the side, so that a browser that goes away in the
/// middle of its request doesn't leave a turn that nobody was told of.
fn run(state: &AppState, machine: &str, id: &str) {
    let (state, machine, id) = (state.clone(), machine.to_owned(), id.to_owned());
    tokio::spawn(async move {
        let running = db::running_chats(&state.pool, Some(&machine)).await;
        let mut running = running.unwrap_or_default().into_iter();
        if let Some(chat) = running.find(|chat| chat.id == id) {
            tell(&state, &machine, chat).await;
        }
    });
}

/// Tells a machine that just connected of the turns it should be running: those it never
/// heard of start, and those it was running say what the server missed.
pub async fn resume(state: &AppState, machine: &str) {
    let running = db::running_chats(&state.pool, Some(machine)).await;
    for chat in running.unwrap_or_default() {
        tell(state, machine, chat).await;
    }
}

/// Ends the turns whose machine was removed, which nothing else would end.
pub async fn orphaned(state: &AppState) {
    let running = db::running_chats(&state.pool, None).await;
    for chat in running.unwrap_or_default() {
        let failed = db::fail_chat(&state.pool, &chat.id, "The machine was removed").await;
        failed.ok();
    }
}

async fn tell(state: &AppState, machine: &str, chat: Running) {
    if !state.cores.connected(machine) {
        return;
    }
    // The machine fetches with a token for this repository alone, which GitHub ends after an hour.
    let token = async {
        let no_repository = || refused("The project has no repository any more");
        let repo = chat.repo.as_deref().ok_or_else(no_repository)?;
        let agent = Agent::named(&chat.agent).ok_or_else(not_found)?;
        let token = github::token(state, &chat.team_id, Some(repo), github::reading()).await?;
        Ok((repo, agent, token))
    };
    let (repo, agent, token) = match token.await {
        Ok(found) => found,
        Err(ApiError(_, why)) => {
            let failed = db::fail_chat(&state.pool, &chat.id, &why).await;
            return failed.unwrap_or_default();
        }
    };
    let message = ToCore::Turn(core_link::Turn {
        repository: repository(repo, chat.branch, token),
        chat: chat.id,
        agent,
        session: chat.session,
        prompt: chat.asked,
        heard: chat.heard as u64,
        hosts: chat.hosts.0,
    });
    state.cores.send(machine, message);
}

fn repository(repo: &str, branch: String, token: String) -> Repository {
    Repository {
        url: format!("https://github.com/{repo}.git"),
        branch,
        token,
    }
}

/// Takes in what a machine tells of a chat of its own.
pub async fn heard(state: &AppState, machine: &str, chat: &str, number: u64, event: Event) {
    let heard = db::hear_chat(&state.pool, machine, chat, number as i64, &event);
    heard.await.ok();
}

/// Everything a chat changed in its folder, as git writes it.
pub async fn diff(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let diff = db::chat_diff(&state.pool, &team_id, &id).await?;
    Ok(Json(json!({"diff": diff.ok_or_else(not_found)?})))
}

/// The branch a chat's work is pushed to: its title, and the end of its id to tell it from a chat called the same.
fn pushed_to(chat: &Chat) -> String {
    let letters = chat.title.to_lowercase();
    let words = letters.split(|c: char| !c.is_ascii_alphanumeric());
    let words: Vec<&str> = words.filter(|word| !word.is_empty()).collect();
    let title: String = words.join("-").chars().take(40).collect();
    let end = &chat.id[chat.id.len() - 4..];
    format!("weacon/{}-{end}", title.trim_end_matches('-'))
}

/// Has the chat's machine commit and push what its folder holds changed, and opens the
/// chat's pull request if it has none.
pub async fn push(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Json(message): Json<Message>,
) -> Result<Json<Value>, ApiError> {
    let chat = own(&state, &user, &team_id, &id).await?;
    let message = message.text.trim();
    if message.is_empty() {
        return Err(refused("Write what the commit should be called"));
    }
    if chat.running {
        return Err(busy());
    }
    let machine = connected(&state, &chat)?;
    let name = chat.machine.as_deref().unwrap_or_default();
    let projects = db::projects(&state.pool, &team_id).await?;
    let project = projects
        .iter()
        .find(|project| project.id == chat.project_id);
    let repo = project.and_then(|project| project.repo.as_deref());
    let repo = repo.ok_or_else(|| refused("The project has no repository any more"))?;

    // Each token may do one thing in this one repository: the machine's pushes, the server's opens the pull request.
    let token = |may| github::token(&state, &team_id, Some(repo), may);
    let pushing = token(json!({"contents": "write"})).await?;
    let opening = token(json!({"pull_requests": "write", "contents": "read"})).await?;
    let to = pushed_to(&chat);
    let asked = |id| ToCore::Push {
        id,
        chat: chat.id.clone(),
        repository: repository(repo, chat.branch.clone(), pushing),
        to: to.clone(),
        message: message.to_owned(),
        name: user.name.clone(),
        mail: user.email.clone(),
    };
    let slow = format!("{name} took too long to push");
    let pushed = crate::machines::answered(&state, (machine, name), &slow, asked).await;
    let commit = pushed.map_err(refused)?;

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
        db::set_chat_pull(&state.pool, &chat.id, opened.number, &opened.html_url).await?;
        pull["number"] = json!(opened.number);
        pull["url"] = json!(opened.html_url);
    }
    Ok(Json(pull))
}
