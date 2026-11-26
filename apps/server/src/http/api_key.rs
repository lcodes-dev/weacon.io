//! The team's API key, which pays for its own agent and, if chosen, for its docs.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::writer::Kind;
use crate::{AppState, db};

#[derive(Deserialize)]
pub struct Key {
    key: String,
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(Key { key }): Json<Key>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(ApiError::invalid("An API key is needed"));
    }
    db::settings::save_api_key(&state.pool, &state.server_key, &team_id, Some(key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let writer = db::settings::writer(&state.pool, &state.server_key, &team_id).await?;
    if writer.is_some_and(|writer| writer.kind == Kind::ClaudeApi) {
        let message = "The team's docs are written with this key. Choose another writer first";
        return Err(ApiError::invalid(message));
    }
    db::settings::save_api_key(&state.pool, &state.server_key, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
