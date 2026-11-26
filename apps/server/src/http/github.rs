use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::github::{self, GithubApp, Status};
use crate::{AppState, db};

/// What the team's GitHub App can reach.
pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Status>> {
    user.member_of(&team_id)?;
    Ok(Json(github::team_status(&state, &team_id).await?))
}

#[derive(Deserialize)]
pub struct Registration {
    /// From GitHub, after the owner confirmed the app.
    code: String,
}

/// Keeps the app GitHub made for the team from its manifest.
pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(registration): Json<Registration>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let path = format!("/app-manifests/{}/conversions", registration.code);
    let app: GithubApp = github::answer(github::request(&state, Method::POST, &path)).await?;
    db::settings::save_github_app(&state.pool, &state.server_key, &team_id, Some(&app)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    db::settings::save_github_app(&state.pool, &state.server_key, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct OfRepo {
    repo: String,
}

#[derive(Deserialize)]
struct Repo {
    default_branch: String,
}

/// The repository's branches, for choosing which to track.
pub async fn branches(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Query(of): Query<OfRepo>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    // The name goes into GitHub's address, so it must be nothing but `owner/name`.
    let plain = |part: &str| {
        part.chars()
            .all(|c| c.is_alphanumeric() || "-_.".contains(c))
    };
    if of.repo.split('/').count() != 2 || !of.repo.split('/').all(plain) {
        return Err(ApiError::not_found());
    }
    let token = github::token(&state, &team_id, None, github::reading()).await?;
    let repo: Repo = github::get(&state, &token, &format!("/repos/{}", of.repo)).await?;
    let branches = github::branches(&state, &token, &of.repo).await?;
    let mut names: Vec<String> = branches.into_keys().collect();
    names.sort();
    Ok(Json(
        json!({"default": repo.default_branch, "branches": names}),
    ))
}
