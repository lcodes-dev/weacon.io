use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::writer::{Kind, Writer};
use crate::{AppState, db};

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(writer): Json<Writer>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let key = db::settings::api_key(&state.pool, &state.server_key, &team_id).await?;
    if writer.kind == Kind::ClaudeApi && key.is_none() {
        return Err(ApiError::invalid("Save the team's API key first"));
    }
    db::settings::save_writer(&state.pool, &team_id, Some(&writer)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    db::settings::save_writer(&state.pool, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
