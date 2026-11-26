use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::db;
use super::db::{Task, TaskFields};
use crate::accounts::db::User;
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::{AppState, docs, live, projects};

type TaskPath = Path<(String, String, String)>;

pub async fn list_tasks(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Vec<Task>>> {
    user.member_of(&team_id)?;
    Ok(Json(db::list(&state.pool, &team_id, &project_id).await?))
}

pub async fn create_task(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(fields): Json<TaskFields>,
) -> Result<Json<Value>> {
    project(&state, &user, &team_id, &project_id).await?;
    let place = (team_id.as_str(), project_id.as_str());
    let (id, _) = super::create(&state, place, &user.id, None, fields).await?;
    Ok(Json(json!({"id": id})))
}

pub async fn update_task(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, task_id)): TaskPath,
    Json(fields): Json<TaskFields>,
) -> Result<StatusCode> {
    found(&state, &user, &team_id, &project_id, &task_id).await?;
    super::update(&state, (&team_id, &project_id), &task_id, fields).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Order {
    status: String,
    tasks: Vec<String>,
}

pub async fn order(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(order): Json<Order>,
) -> Result<StatusCode> {
    project(&state, &user, &team_id, &project_id).await?;
    super::check_status(&order.status)?;
    db::order(&state.pool, &project_id, &order.status, &order.tasks).await?;
    live::team(&state, &team_id, Topic::Tasks).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn docs(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, task_id)): TaskPath,
) -> Result<Json<Vec<String>>> {
    let task = found(&state, &user, &team_id, &project_id, &task_id).await?;
    let question = format!("{}\n{}", task.title, task.text);
    let mut pages: Vec<String> = Vec::new();
    for part in docs::search::search(&state, &project_id, &question).await? {
        if !task.docs.contains(&part.doc_id) && !pages.contains(&part.doc_id) {
            pages.push(part.doc_id);
        }
    }
    pages.truncate(3);
    Ok(Json(pages))
}

pub async fn delete_task(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, task_id)): TaskPath,
) -> Result<StatusCode> {
    let task = found(&state, &user, &team_id, &project_id, &task_id).await?;
    if task.asker_id.as_deref() != Some(&user.id) {
        user.owner_of(&team_id)?;
    }
    db::delete(&state.pool, &project_id, &task_id).await?;
    live::team(&state, &team_id, Topic::Tasks).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn project(state: &AppState, user: &User, team_id: &str, project_id: &str) -> Result<()> {
    user.member_of(team_id)?;
    let projects = projects::db::list(&state.pool, team_id).await?;
    if !projects.iter().any(|project| project.id == project_id) {
        return Err(ApiError::not_found());
    }
    Ok(())
}

async fn found(
    state: &AppState,
    user: &User,
    team_id: &str,
    project_id: &str,
    task_id: &str,
) -> Result<Task> {
    user.member_of(team_id)?;
    let tasks = db::list(&state.pool, team_id, project_id).await?;
    let task = tasks.into_iter().find(|task| task.id == task_id);
    task.ok_or_else(ApiError::not_found)
}
