use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::db::{Map, Project};
use super::github::{GithubApp, Status};
use super::maps::MapBranches;
use super::{db, github};
use crate::accounts::db::User;
use crate::error::{ApiError, Result};
use crate::jobs::Job;
use crate::live::Topic;
use crate::{AppState, jobs, live, now, settings};

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
        let projects = db::list(&state.pool, team_id).await?;
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

pub async fn list_projects(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Vec<Project>>> {
    user.member_of(&team_id)?;
    Ok(Json(db::list(&state.pool, &team_id).await?))
}

pub async fn create_project(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(fields): Json<Fields>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    let project = fields.checked(&state, &team_id, None).await?;
    let id = db::create(
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

pub async fn update_project(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(fields): Json<Fields>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    let project = fields.checked(&state, &team_id, Some(&project_id)).await?;
    let changed = db::update(
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

pub async fn delete_project(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    for file in db::delete(&state.pool, &team_id, &project_id).await? {
        tokio::fs::remove_file(state.files.join(file)).await.ok();
    }
    live::team(&state, &team_id, Topic::Projects).await;
    live::team(&state, &team_id, Topic::Chats).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Which {
    branch: String,
}

pub async fn show_map(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(which): Query<Which>,
) -> Result<Json<Map>> {
    user.member_of(&team_id)?;
    let map = db::find_map(&state.pool, &team_id, &project_id, &which.branch).await?;
    Ok(Json(map.ok_or_else(ApiError::not_found)?))
}

pub async fn show_github(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Status>> {
    user.member_of(&team_id)?;
    Ok(Json(github::team_status(&state, &team_id).await?))
}

#[derive(Deserialize)]
pub struct Registration {
    code: String,
}

pub async fn create_github(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(registration): Json<Registration>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let path = format!("/app-manifests/{}/conversions", registration.code);
    let app: GithubApp = github::answer(github::request(&state, Method::POST, &path)).await?;
    settings::db::save_github_app(&state.pool, &state.server_key, &team_id, Some(&app)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_github(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    settings::db::save_github_app(&state.pool, &state.server_key, &team_id, None).await?;
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

pub async fn repo_branches(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Query(of): Query<OfRepo>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
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
