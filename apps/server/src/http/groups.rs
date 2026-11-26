use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::groups::Group;
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::http::chats::After;
use crate::jobs::AnswerAsWeacon;
use crate::{AppState, db, group_calls, live};

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Group>>> {
    user.member_of(&team_id)?;
    Ok(Json(
        db::groups::list(&state.pool, &team_id, &user.id).await?,
    ))
}

#[derive(Deserialize)]
pub struct NewGroup {
    members: Vec<String>,
    name: Option<String>,
    project_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(mut group): Json<NewGroup>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    group.members.push(user.id.clone());
    check_members(&state, &team_id, &group.members).await?;
    let about = group.project_id.as_deref();
    check_project(&state, &team_id, about).await?;
    let name = named(&group.name);
    let id = db::groups::create(&state.pool, &team_id, name, about, &group.members).await?;
    live::group(&state, &id).await;
    Ok(Json(json!({"id": id})))
}

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, group_id)): Path<(String, String)>,
    Query(after): Query<After>,
) -> Result<Json<Value>> {
    let group = found(&state, &user, &team_id, &group_id).await?;
    let mut working = db::groups::working(&state.pool, &group_id).await?;
    if db::jobs::exists(&state.pool, &AnswerAsWeacon::key(&group_id)).await? {
        working.push((String::new(), group_calls::WEACON.into()));
    }
    let messages = db::groups::messages(&state.pool, &group_id, &after.after).await?;
    db::groups::read(&state.pool, &group_id, &user.id).await?;
    let mut group = json!(group);
    group["messages"] = json!(messages);
    group["working"] = json!(working);
    Ok(Json(group))
}

#[derive(Deserialize)]
pub struct Changes {
    name: Option<String>,
    project_id: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path((team_id, group_id)): Path<(String, String)>,
    Json(changes): Json<Changes>,
) -> Result<StatusCode> {
    let group = found(&state, &user, &team_id, &group_id).await?;
    let about = changes.project_id.as_deref();
    if group.project_id.is_some() && about.is_some() && group.project_id.as_deref() != about {
        return Err(ApiError::invalid("A group keeps the project it was given"));
    }
    check_project(&state, &team_id, about).await?;
    db::groups::change(&state.pool, &group_id, named(&changes.name), about).await?;
    live::group(&state, &group_id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Joining {
    user_id: String,
}

pub async fn add_member(
    State(state): State<AppState>,
    user: User,
    Path((team_id, group_id)): Path<(String, String)>,
    Json(joining): Json<Joining>,
) -> Result<StatusCode> {
    found(&state, &user, &team_id, &group_id).await?;
    check_members(&state, &team_id, std::slice::from_ref(&joining.user_id)).await?;
    db::groups::join(&state.pool, &group_id, &joining.user_id).await?;
    live::group(&state, &group_id).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn leave(
    State(state): State<AppState>,
    user: User,
    Path((team_id, group_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    found(&state, &user, &team_id, &group_id).await?;
    db::groups::leave(&state.pool, &team_id, Some(&group_id), &user.id).await?;
    live::group(&state, &group_id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Message {
    text: String,
}

pub async fn write(
    State(state): State<AppState>,
    user: User,
    Path((team_id, group_id)): Path<(String, String)>,
    Json(message): Json<Message>,
) -> Result<Json<Value>> {
    let group = found(&state, &user, &team_id, &group_id).await?;
    let text = message.text.trim();
    if text.is_empty() {
        return Err(ApiError::invalid("Write something"));
    }
    db::groups::write(&state.pool, &group_id, &user.id, text).await?;
    let refused = group_calls::call_tagged(&state, &user, &team_id, &group, text).await?;
    live::group(&state, &group_id).await;
    Ok(Json(json!({"refused": refused})))
}

async fn found(state: &AppState, user: &User, team_id: &str, group_id: &str) -> Result<Group> {
    user.member_of(team_id)?;
    let groups = db::groups::list(&state.pool, team_id, &user.id).await?;
    let group = groups.into_iter().find(|group| group.id == group_id);
    group.ok_or_else(ApiError::not_found)
}

async fn check_members(state: &AppState, team_id: &str, people: &[String]) -> Result<()> {
    let members = db::teams::member_ids(&state.pool, team_id).await?;
    if !people.iter().all(|person| members.contains(person)) {
        return Err(ApiError::invalid(
            "A group's members are members of its team",
        ));
    }
    Ok(())
}

async fn check_project(state: &AppState, team_id: &str, project_id: Option<&str>) -> Result<()> {
    let projects = db::projects::list(&state.pool, team_id).await?;
    if !project_id.is_none_or(|id| projects.iter().any(|project| project.id == id)) {
        return Err(ApiError::invalid("A group is about a project of its team"));
    }
    Ok(())
}

fn named(name: &Option<String>) -> Option<&str> {
    name.as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}
