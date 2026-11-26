use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

use crate::db::maps::Map;
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::{AppState, db};

#[derive(Deserialize)]
pub struct Which {
    branch: String,
}

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(which): Query<Which>,
) -> Result<Json<Map>> {
    user.member_of(&team_id)?;
    let map = db::maps::find(&state.pool, &team_id, &project_id, &which.branch).await?;
    Ok(Json(map.ok_or_else(ApiError::not_found)?))
}
