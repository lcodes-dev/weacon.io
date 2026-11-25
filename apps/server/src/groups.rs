//! Groups: some members of a team and what they write to each other. Only its members
//! read a group, write in it, or see that it is there.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use core_link::Agent;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::chats::{self, After};
use crate::db::{self, Group, User};
use crate::teams::{membership, refused};
use crate::{ApiError, AppState, not_found, weacon};

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Group>>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(db::groups(&state.pool, &team_id, &user.id).await?))
}

/// A group the person is in. One they aren't in looks like one that doesn't exist.
async fn found(state: &AppState, user: &User, team_id: &str, id: &str) -> Result<Group, ApiError> {
    membership(user, team_id)?;
    let groups = db::groups(&state.pool, team_id, &user.id).await?;
    groups
        .into_iter()
        .find(|group| group.id == id)
        .ok_or_else(not_found)
}

/// Whether each of `people` is a member of the team.
async fn members(state: &AppState, team_id: &str, people: &[String]) -> Result<(), ApiError> {
    let members = db::members(&state.pool, team_id).await?;
    let known = |person: &String| members.iter().any(|member| &member.id == person);
    match people.iter().all(known) {
        true => Ok(()),
        false => Err(refused("A group's members are members of its team")),
    }
}

/// Whether the project, if one is named, is the team's.
async fn project(state: &AppState, team_id: &str, id: Option<&str>) -> Result<(), ApiError> {
    let projects = db::projects(&state.pool, team_id).await?;
    match id.is_none_or(|id| projects.iter().any(|project| project.id == id)) {
        true => Ok(()),
        false => Err(refused("A group is about a project of its team")),
    }
}

/// A name as it is kept: none when nothing is written.
fn named(name: &Option<String>) -> Option<&str> {
    name.as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

#[derive(Deserialize)]
pub struct New {
    /// The others in it: who makes it is in it too.
    members: Vec<String>,
    name: Option<String>,
    project_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(mut new): Json<New>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    new.members.push(user.id.clone());
    members(&state, &team_id, &new.members).await?;
    let about = new.project_id.as_deref();
    project(&state, &team_id, about).await?;
    let id = db::create_group(&state.pool, &team_id, named(&new.name), about, &new.members).await?;
    Ok(Json(json!({"id": id})))
}

/// The group and its messages, or those after one, which the person has then read.
pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Query(after): Query<After>,
) -> Result<Json<Value>, ApiError> {
    let group = found(&state, &user, &team_id, &id).await?;
    // Asked first, so that an answer is never missed between the two.
    let mut working = db::group_working(&state.pool, &id).await?;
    if state.weacon.lock().unwrap().contains(&id) {
        working.push((String::new(), weacon::NAME.into()));
    }
    let messages = db::group_messages(&state.pool, &id, &after.after).await?;
    db::read_group(&state.pool, &id, &user.id).await?;
    let mut group = json!(group);
    group["messages"] = json!(messages);
    group["working"] = json!(working);
    Ok(Json(group))
}

#[derive(Deserialize)]
pub struct Changed {
    name: Option<String>,
    project_id: Option<String>,
}

/// Names the group, and gives it its project while it has none.
pub async fn change(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Json(changed): Json<Changed>,
) -> Result<StatusCode, ApiError> {
    let group = found(&state, &user, &team_id, &id).await?;
    let about = changed.project_id.as_deref();
    if group.project_id.is_some() && about.is_some() && group.project_id.as_deref() != about {
        return Err(refused("A group keeps the project it was given"));
    }
    project(&state, &team_id, about).await?;
    db::change_group(&state.pool, &id, named(&changed.name), about).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Joining {
    user_id: String,
}

pub async fn add(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Json(joining): Json<Joining>,
) -> Result<StatusCode, ApiError> {
    found(&state, &user, &team_id, &id).await?;
    members(&state, &team_id, std::slice::from_ref(&joining.user_id)).await?;
    db::join_group(&state.pool, &id, &joining.user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn leave(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    found(&state, &user, &team_id, &id).await?;
    db::leave_groups(&state.pool, &team_id, Some(&id), &user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Message {
    text: String,
}

/// Writes a message, and has each agent it tags answer: the writer's own, and the team's.
/// What it gives back is why a tagged agent doesn't run, for each that doesn't.
pub async fn write(
    State(state): State<AppState>,
    user: User,
    Path((team_id, id)): Path<(String, String)>,
    Json(message): Json<Message>,
) -> Result<Json<Value>, ApiError> {
    let group = found(&state, &user, &team_id, &id).await?;
    let text = message.text.trim();
    if text.is_empty() {
        return Err(refused("Write something"));
    }
    db::write_group(&state.pool, &id, &user.id, text).await?;
    let mut refused = Vec::new();
    for agent in Agent::ALL {
        if chats::tags(text, agent.program()) {
            refused.extend(chats::called(&state, &user, &team_id, &group, agent).await?);
        }
    }
    if chats::tags(text, weacon::NAME) {
        refused.extend(weacon::called(&state, &user, &team_id, &group).await?);
    }
    refused.dedup();
    Ok(Json(json!({"refused": refused})))
}
