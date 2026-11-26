use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db};

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Value>> {
    let team = user.member_of(&team_id)?;
    let members = db::teams::members(&state.pool, &team_id).await?;
    let invites = db::teams::invites(&state.pool, &team_id).await?;
    let writer = db::settings::writer(&state.pool, &state.server_key, &team_id).await?;
    let api_key = db::settings::api_key(&state.pool, &state.server_key, &team_id).await?;
    Ok(Json(json!({
        "name": team.name,
        "members": members,
        "invites": invites,
        "writer": writer.map(|writer| writer.public()),
        "has_key": api_key.is_some(),
    })))
}

#[derive(Deserialize)]
pub struct Rename {
    name: String,
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(rename): Json<Rename>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let name = rename.name.trim();
    if name.is_empty() {
        return Err(ApiError::invalid("A team needs a name"));
    }
    db::teams::rename(&state.pool, &team_id, name).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_member(
    State(state): State<AppState>,
    user: User,
    Path((team_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    if user_id == user.id {
        user.member_of(&team_id)?;
    } else {
        user.owner_of(&team_id)?;
    }
    if !db::teams::remove_member(&state.pool, &team_id, &user_id).await? {
        return Err(ApiError::invalid("The team's owner can't leave"));
    }
    live::team(&state, &team_id, Topic::Groups).await;
    Ok(StatusCode::NO_CONTENT)
}
