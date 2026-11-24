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

#[derive(Deserialize)]
pub struct Answer {
    host: String,
    allowed: bool,
    /// The id the agent's program asked under, when it is waiting for the answer.
    ask: Option<String>,
}

/// The owner's answer about a host the chat's commands wanted. An allowed host joins the
/// chat's hosts. A program that waits is answered; one that was refused is told in a next
/// turn to try again.
pub async fn answer(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Json(answer): Json<Answer>,
) -> Result<StatusCode, ApiError> {
    let chat = own(&state, &user, &team_id, &id).await?;
    let Answer { host, allowed, ask } = answer;
    if !core_link::host(&host) {
        return Err(refused(
            "A host is an exact name, such as registry.npmjs.org",
        ));
    }
    let Some(machine) = &chat.machine_id else {
        return Err(refused(
            "This chat's machine was removed, and its folder with it",
        ));
    };
    match ask {
        Some(ask) => {
            if !state.cores.connected(machine) {
                return Err(refused(
                    "The machine is away, so its agent can't be answered from here",
                ));
            }
            if !db::answer_chat(&state.pool, &id, Some(&ask), &host, allowed).await? {
                return Err(refused("The agent isn't waiting for that any more"));
            }
            let chat = id.clone();
            let answer = ToCore::Answer { chat, ask, allowed };
            state.cores.send(machine, answer);
        }
        None if allowed => {
            db::answer_chat(&state.pool, &id, None, &host, true).await?;
            let again = format!("{host} can be reached now. Try again what needed it.");
            if db::ask_chat(&state.pool, &id, &again).await? {
                run(&state, &id).await;
            }
        }
        None => return Err(refused("No agent is waiting to be refused")),
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
    let token = github::token(state, &turn.team_id, Some(repo), github::reading());
    let token = match token.await {
        Ok(token) => token,
        Err(ApiError(_, why)) => {
            return db::fail_chat(&state.pool, &turn.id, &why)
                .await
                .unwrap_or_default();
        }
    };
    let repository = repository(repo, turn.branch, token);
    let message = ToCore::Turn(core_link::Turn {
        chat: turn.id,
        agent,
        session: turn.session,
        prompt: turn.asked,
        repository,
        heard: turn.heard as u64,
        hosts: turn.hosts.0,
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
        let message = "The agent is still working. Stop it, or wait for it to finish";
        return Err(ApiError(StatusCode::CONFLICT, message.into()));
    }
    let (Some(machine), Some(name)) = (&chat.machine_id, &chat.machine) else {
        return Err(refused(
            "This chat's machine was removed, and its folder with it",
        ));
    };
    if !state.cores.connected(machine) {
        return Err(refused(format!(
            "{name} is away, and the chat's files are on it"
        )));
    }
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
