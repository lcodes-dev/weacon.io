use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::projects::Project;
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::jobs::{self, Job, MapBranches};
use crate::live::{self, Topic};
use crate::{AppState, db, github, now};

#[derive(Deserialize)]
pub struct Fields {
    name: String,
    key: String,
    repo: Option<String>,
    #[serde(default)]
    branches: Vec<String>,
}

struct Checked<'a> {
    name: &'a str,
    key: String,
    repo: Option<&'a str>,
    branches: &'a [String],
}

impl Fields {
    async fn checked(
        &self,
        state: &AppState,
        team_id: &str,
        project_id: Option<&str>,
    ) -> Result<Checked<'_>> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(ApiError::invalid("A project needs a name"));
        }
        let key = self.key.trim().to_uppercase();
        if !(2..=5).contains(&key.len()) || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(ApiError::invalid(
                "A project's key is two to five letters or digits",
            ));
        }
        let projects = db::projects::list(&state.pool, team_id).await?;
        let taken = projects
            .iter()
            .find(|other| other.key == key && Some(other.id.as_str()) != project_id);
        if let Some(other) = taken {
            return Err(ApiError::invalid(format!(
                "{} already has the key {key}",
                other.name
            )));
        }
        let repo = self.repo.as_deref().filter(|repo| !repo.is_empty());
        if let Some(repo) = repo {
            let reachable = github::team_status(state, team_id).await?.repos;
            if !reachable.iter().any(|reachable| reachable == repo) {
                let message = format!("The team's GitHub App can't reach {repo}");
                return Err(ApiError::invalid(message));
            }
        }
        let branches = if repo.is_some() {
            &self.branches[..]
        } else {
            &[]
        };
        Ok(Checked {
            name,
            key,
            repo,
            branches,
        })
    }
}

pub async fn list(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Project>>> {
    user.member_of(&team_id)?;
    Ok(Json(db::projects::list(&state.pool, &team_id).await?))
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(fields): Json<Fields>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    let project = fields.checked(&state, &team_id, None).await?;
    let id = db::projects::create(
        &state.pool,
        &team_id,
        project.name,
        &project.key,
        project.repo,
        project.branches,
        now(),
    )
    .await?;
    jobs::dispatch(&state, Job::MapBranches(MapBranches)).await?;
    live::team(&state, &team_id, Topic::Projects).await;
    Ok(Json(json!({"id": id})))
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(fields): Json<Fields>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    let project = fields.checked(&state, &team_id, Some(&project_id)).await?;
    let changed = db::projects::update(
        &state.pool,
        &team_id,
        &project_id,
        project.name,
        &project.key,
        project.repo,
        project.branches,
    )
    .await?;
    if changed.ok_or_else(ApiError::not_found)? {
        jobs::dispatch(&state, Job::MapBranches(MapBranches)).await?;
    }
    live::team(&state, &team_id, Topic::Projects).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    for file in db::projects::delete(&state.pool, &team_id, &project_id).await? {
        tokio::fs::remove_file(state.files.join(file)).await.ok();
    }
    live::team(&state, &team_id, Topic::Projects).await;
    live::team(&state, &team_id, Topic::Chats).await;
    Ok(StatusCode::NO_CONTENT)
}
