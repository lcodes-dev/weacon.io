//! Public links to a copy of a project's user docs, which anyone who has one may read.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::auth::random_token;
use crate::db::shares::{Share, Shared};
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::{AppState, db, now};

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Vec<Share>>> {
    user.member_of(&team_id)?;
    Ok(Json(
        db::shares::list(&state.pool, &team_id, &project_id).await?,
    ))
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    let pages = db::docs::list(&state.pool, &project_id).await?;
    let pages: Vec<(&str, &str)> = pages
        .iter()
        .filter(|page| page.section == "works")
        .map(|page| (page.title.as_str(), page.markdown.as_str()))
        .collect();
    if pages.is_empty() {
        return Err(ApiError::invalid("This project has no user docs to share"));
    }
    let token = random_token();
    let shared = db::shares::share(
        &state.pool,
        &team_id,
        &project_id,
        &token,
        &user.id,
        now(),
        &pages,
    );
    if !shared.await? {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({"token": token})))
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, token)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    if !db::shares::end(&state.pool, &team_id, &project_id, &token).await? {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn show(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Shared>> {
    let shared = db::shares::find(&state.pool, &token).await?;
    Ok(Json(shared.ok_or_else(ApiError::not_found)?))
}
