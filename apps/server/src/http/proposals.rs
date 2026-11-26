use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db};

#[derive(Deserialize)]
pub struct Accepted {
    version: Option<String>,
    markdown: Option<String>,
}

pub async fn accept(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, proposal_id)): Path<(String, String, String)>,
    Json(accepted): Json<Accepted>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    let accepted = db::proposals::accept(
        &state.pool,
        &team_id,
        &project_id,
        &proposal_id,
        accepted.version.as_deref(),
        accepted.markdown.as_deref(),
        &user.id,
    );
    if !accepted.await?.ok_or_else(ApiError::not_found)? {
        let message = "The page changed after this proposal was shown. Look at it again";
        return Err(ApiError::conflict(message));
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, proposal_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    if !db::proposals::delete(&state.pool, &team_id, &project_id, &proposal_id).await? {
        return Err(ApiError::not_found());
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}
