//! Tasks: what a project needs done, who does it and how far it is.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, Task, TaskFields, User};
use crate::teams::{membership, owned, refused};
use crate::{ApiError, AppState, not_found};

type Place = Path<(String, String, String)>;

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
pub async fn checked(
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
    let fields = checked(&state, &team_id, fields).await?;
    if fields.title.is_none() {
        return Err(refused("A task needs a title"));
    }
    let id = db::create_task(&state.pool, &project_id, &user.id, None, &fields).await?;
    Ok(Json(json!({"id": id})))
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
    let fields = checked(&state, &place.0, fields).await?;
    db::change_task(&state.pool, &place.1, &place.2, &fields).await?;
    Ok(StatusCode::NO_CONTENT)
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
    if !["todo", "doing", "done"].contains(&order.status.as_str()) {
        return Err(refused("A task is to do, doing or done"));
    }
    db::order_tasks(&state.pool, &project_id, &order.status, &order.tasks).await?;
    Ok(StatusCode::NO_CONTENT)
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
    Ok(StatusCode::NO_CONTENT)
}
