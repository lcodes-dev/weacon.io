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
    /// What the project's tasks are named by: two to five letters or digits.
    key: String,
    /// `owner/name`; missing or empty for a project with no repository.
    repo: Option<String>,
    /// The branches to map.
    #[serde(default)]
    branches: Vec<String>,
}

impl Fields {
    /// The trimmed name, the key no other project of the team has, and the repository, which
    /// the team's GitHub App must reach. `project_id` is the project being changed, if any.
    async fn checked(
        &self,
        state: &AppState,
        team_id: &str,
        project_id: Option<&str>,
    ) -> Result<(&str, String, Option<&str>), ApiError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(refused("A project needs a name"));
        }
        let key = self.key.trim().to_uppercase();
        if !(2..=5).contains(&key.len()) || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(refused("A project's key is two to five letters or digits"));
        }
        let projects = db::projects(&state.pool, team_id).await?;
        if let Some(other) = projects
            .iter()
            .find(|other| other.key == key && Some(other.id.as_str()) != project_id)
        {
            return Err(refused(format!("{} already has the key {key}", other.name)));
        }
        let repo = self.repo.as_deref().filter(|repo| !repo.is_empty());
        if let Some(repo) = repo {
            let reachable = github::team_status(state, team_id).await?.repos;
            if !reachable.iter().any(|reachable| reachable == repo) {
                return Err(refused(format!("The team's GitHub App can't reach {repo}")));
            }
        }
        Ok((name, key, repo))
    }

    /// The branches to map: none without a repository.
    fn branches(&self, repo: Option<&str>) -> &[String] {
        if repo.is_some() { &self.branches } else { &[] }
    }
}

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Project>>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(db::projects(&state.pool, &team_id).await?))
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(fields): Json<Fields>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let (name, key, repo) = fields.checked(&state, &team_id, None).await?;
    let branches = fields.branches(repo);
    let id = db::create_project(&state.pool, &team_id, name, &key, repo, branches, now()).await?;
    state.map_wake.notify_one();
    Ok(Json(json!({"id": id})))
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(fields): Json<Fields>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    let (name, key, repo) = fields.checked(&state, &team_id, Some(&project_id)).await?;
    let branches = fields.branches(repo);
    let changed = db::update_project(
        &state.pool,
        &team_id,
        &project_id,
        name,
        &key,
        repo,
        branches,
    )
    .await?;
    if changed.ok_or_else(crate::not_found)? {
        state.map_wake.notify_one();
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    for file in db::delete_project(&state.pool, &team_id, &project_id).await? {
        tokio::fs::remove_file(state.files.join(file)).await.ok();
    }
    Ok(StatusCode::NO_CONTENT)
}
