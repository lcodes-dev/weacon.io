use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, Project, User};
use crate::teams::{membership, owned, refused};
use crate::{ApiError, AppState, github, now};

#[derive(Deserialize)]
pub struct Fields {
    name: String,
    /// `owner/name`; missing or empty for a project with no repository.
    repo: Option<String>,
    /// The branches to map.
    #[serde(default)]
    branches: Vec<String>,
}

impl Fields {
    /// The trimmed name and the repository, which the team's GitHub App must reach.
    async fn checked(
        &self,
        state: &AppState,
        team_id: i64,
    ) -> Result<(&str, Option<&str>), ApiError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(refused("A project needs a name"));
        }
        let repo = self.repo.as_deref().filter(|repo| !repo.is_empty());
        if let Some(repo) = repo {
            let reachable = github::team_status(state, team_id).await?.repos;
            if !reachable.iter().any(|reachable| reachable == repo) {
                return Err(refused(format!("The team's GitHub App can't reach {repo}")));
            }
        }
        Ok((name, repo))
    }

    /// Makes the project track the chosen branches, and has them mapped now.
    async fn track(&self, state: &AppState, project_id: i64, reset: bool) -> sqlx::Result<()> {
        let has_repo = self.repo.as_deref().is_some_and(|repo| !repo.is_empty());
        let branches: &[String] = if has_repo { &self.branches } else { &[] };
        db::track_branches(&state.pool, project_id, branches, reset).await?;
        state.map_wake.notify_one();
        Ok(())
    }
}

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
) -> Result<Json<Vec<Project>>, ApiError> {
    membership(&user, team_id)?;
    Ok(Json(db::projects(&state.pool, team_id).await?))
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
    Json(fields): Json<Fields>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, team_id)?;
    let (name, repo) = fields.checked(&state, team_id).await?;
    let id = db::create_project(&state.pool, team_id, name, repo, now()).await?;
    fields.track(&state, id, false).await?;
    Ok(Json(json!({"id": id})))
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(i64, i64)>,
    Json(fields): Json<Fields>,
) -> Result<StatusCode, ApiError> {
    membership(&user, team_id)?;
    let (name, repo) = fields.checked(&state, team_id).await?;
    let old_repo = db::project_repo(&state.pool, team_id, project_id).await?;
    let old_repo = old_repo.ok_or_else(crate::not_found)?;
    db::update_project(&state.pool, team_id, project_id, name, repo).await?;
    // Another repository's maps say nothing about this one.
    fields
        .track(&state, project_id, old_repo.as_deref() != repo)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    db::delete_project(&state.pool, team_id, project_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
