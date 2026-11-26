//! Tasks: what a project needs done, who does it and how far it is.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, Task, TaskFields, User};
use crate::teams::{membership, owned, refused};
use crate::{ApiError, AppState, live, not_found, search};

type Place = Path<(String, String, String)>;

pub const STATUSES: [&str; 3] = ["todo", "doing", "done"];

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Vec<Task>>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(db::tasks(&state.pool, &team_id, &project_id).await?))
}

/// Whether the project is one of a team the person is in.
async fn project(
    state: &AppState,
    user: &User,
    team_id: &str,
    project_id: &str,
) -> Result<(), ApiError> {
    membership(user, team_id)?;
    let projects = db::projects(&state.pool, team_id).await?;
    let found = projects.iter().any(|project| project.id == project_id);
    found.then_some(()).ok_or_else(not_found)
}

/// The fields as they are kept, or why they can't be.
async fn checked(
    state: &AppState,
    team_id: &str,
    mut fields: TaskFields,
) -> Result<TaskFields, ApiError> {
    if let Some(title) = &mut fields.title {
        *title = title.trim().into();
        if title.is_empty() {
            return Err(refused("A task needs a title"));
        }
    }
    if let Some(checks) = &mut fields.checks {
        checks.retain(|check| !check.text.trim().is_empty());
    }
    if let Some(status) = &fields.status
        && !STATUSES.contains(&status.as_str())
    {
        return Err(refused("A task is todo, doing or done"));
    }
    if let Some(Some(owner)) = &fields.owner_id {
        let members = db::members(&state.pool, team_id).await?;
        if !members.iter().any(|member| &member.id == owner) {
            return Err(refused("A task's owner is a member of the team"));
        }
    }
    Ok(fields)
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(fields): Json<TaskFields>,
) -> Result<Json<Value>, ApiError> {
    project(&state, &user, &team_id, &project_id).await?;
    let place = (team_id.as_str(), project_id.as_str());
    let (id, _) = made(&state, place, &user.id, None, fields).await?;
    Ok(Json(json!({"id": id})))
}

/// Makes a task in a team's project for a person, from a request of theirs or through the
/// agent of their chat `chat_id`, and gives its id and number.
pub async fn made(
    state: &AppState,
    (team_id, project_id): (&str, &str),
    user_id: &str,
    chat_id: Option<&str>,
    fields: TaskFields,
) -> Result<(String, i64), ApiError> {
    let fields = checked(state, team_id, fields).await?;
    if fields.title.is_none() {
        return Err(refused("A task needs a title"));
    }
    let task = db::create_task(&state.pool, project_id, user_id, chat_id, &fields).await?;
    live::team(state, team_id, "tasks").await;
    Ok(task)
}

/// The task at an address, for a member of its team.
async fn found(
    state: &AppState,
    user: &User,
    (team_id, project_id, task_id): &(String, String, String),
) -> Result<Task, ApiError> {
    membership(user, team_id)?;
    let tasks = db::tasks(&state.pool, team_id, project_id).await?;
    let task = tasks.into_iter().find(|task| &task.id == task_id);
    task.ok_or_else(not_found)
}

pub async fn change(
    State(state): State<AppState>,
    user: User,
    Path(place): Place,
    Json(fields): Json<TaskFields>,
) -> Result<StatusCode, ApiError> {
    found(&state, &user, &place).await?;
    changed(&state, (&place.0, &place.1), &place.2, fields).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Changes what `fields` gives of a task of a team's project, and gives them as they were kept.
pub async fn changed(
    state: &AppState,
    (team_id, project_id): (&str, &str),
    task_id: &str,
    fields: TaskFields,
) -> Result<TaskFields, ApiError> {
    let fields = checked(state, team_id, fields).await?;
    db::change_task(&state.pool, project_id, task_id, &fields).await?;
    live::team(state, team_id, "tasks").await;
    Ok(fields)
}

#[derive(Deserialize)]
pub struct Order {
    status: String,
    /// The tasks of that status, in their order.
    tasks: Vec<String>,
}

/// Puts tasks in a status, in the order given. A task of another project is left as it is.
pub async fn order(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(order): Json<Order>,
) -> Result<StatusCode, ApiError> {
    project(&state, &user, &team_id, &project_id).await?;
    if !STATUSES.contains(&order.status.as_str()) {
        return Err(refused("A task is todo, doing or done"));
    }
    db::order_tasks(&state.pool, &project_id, &order.status, &order.tasks).await?;
    live::team(&state, &team_id, "tasks").await;
    Ok(StatusCode::NO_CONTENT)
}

/// Up to three pages of the docs that the task's title and text are about, and that it
/// doesn't touch yet, for a person to pick from.
pub async fn docs(
    State(state): State<AppState>,
    user: User,
    Path(place): Place,
) -> Result<Json<Vec<String>>, ApiError> {
    let task = found(&state, &user, &place).await?;
    let question = format!("{}\n{}", task.title, task.text);
    let mut pages: Vec<String> = Vec::new();
    for part in search::search(&state, &place.1, &question).await? {
        if !task.docs.contains(&part.doc_id) && !pages.contains(&part.doc_id) {
            pages.push(part.doc_id);
        }
    }
    pages.truncate(3);
    Ok(Json(pages))
}

/// Only who asked for a task, or the team's owner, deletes it.
pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path(place): Place,
) -> Result<StatusCode, ApiError> {
    let task = found(&state, &user, &place).await?;
    if task.asker_id.as_deref() != Some(&user.id) {
        owned(&user, &place.0)?;
    }
    db::delete_task(&state.pool, &place.1, &place.2).await?;
    live::team(&state, &place.0, "tasks").await;
    Ok(StatusCode::NO_CONTENT)
}
