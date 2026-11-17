use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::db::{self, Project, User};
use crate::teams::{membership, owned, refused};
use crate::{ApiError, AppState, github, now};

#[derive(Deserialize)]
pub struct Fields {
    name: String,
    /// `owner/name`; missing or empty for a project with no repository.
    repo: Option<String>,
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
) -> Result<Json<Project>, ApiError> {
    membership(&user, team_id)?;
    let (name, repo) = fields.checked(&state, team_id).await?;
    let project = db::create_project(&state.pool, team_id, name, repo, now()).await?;
    Ok(Json(project))
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(i64, i64)>,
    Json(fields): Json<Fields>,
) -> Result<StatusCode, ApiError> {
    membership(&user, team_id)?;
    let (name, repo) = fields.checked(&state, team_id).await?;
    if !db::update_project(&state.pool, team_id, project_id, name, repo).await? {
        return Err(ApiError(StatusCode::NOT_FOUND, "not found".into()));
    }
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
