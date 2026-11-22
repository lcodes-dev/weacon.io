use std::collections::HashMap;
use std::fmt::Display;
use std::sync::LazyLock;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Client, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{self, GithubApp, User};
use crate::teams::{membership, owned};
use crate::{ApiError, AppState, now};

/// What a team's GitHub App can reach. All empty when the team has no app.
#[derive(Default, Serialize)]
pub struct Status {
    /// The app's name on GitHub.
    app: Option<String>,
    /// The account it is installed on, once the owner has chosen repositories.
    account: Option<String>,
    /// As `owner/name`.
    pub repos: Vec<String>,
}

#[derive(Deserialize)]
struct Installation {
    id: i64,
    account: Account,
}

#[derive(Deserialize)]
struct Account {
    login: String,
}

#[derive(Deserialize)]
struct Token {
    token: String,
}

#[derive(Deserialize)]
struct Page {
    repositories: Vec<Repository>,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
}

/// Larger archives are refused, since one is held in memory while it is read.
const MAX_ARCHIVE: usize = 200_000_000;

/// The time limits keep one stalled call from holding up the map's worker.
static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent("weacon")
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .build()
        .unwrap()
});

fn request(state: &AppState, method: Method, path: &str) -> RequestBuilder {
    CLIENT.request(method, format!("{}{path}", state.github_api))
}

fn failed(error: impl Display) -> ApiError {
    tracing::error!("GitHub call failed: {error}");
    let message = "GitHub refused. Check that the team's GitHub App still exists.";
    ApiError(StatusCode::BAD_GATEWAY, message.into())
}

async fn answer<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, ApiError> {
    let answer = async { request.send().await?.error_for_status()?.json().await };
    answer.await.map_err(|error: reqwest::Error| failed(error))
}

/// Signs the short-lived token that proves the server is the app.
fn app_token(app: &GithubApp) -> Result<String, ApiError> {
    // GitHub asks for a start in the past, against clock drift, and at most 10 minutes.
    let claims = json!({"iss": app.id.to_string(), "iat": now() - 60, "exp": now() + 300});
    EncodingKey::from_rsa_pem(app.pem.as_bytes())
        .and_then(|key| jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key))
        .map_err(failed)
}

/// The account the app is installed on and a token to act there, once it is installed.
/// With `only`, the token reads that one repository's files and nothing else.
async fn installation(
    state: &AppState,
    app: &GithubApp,
    only: Option<&str>,
) -> Result<Option<(String, String)>, ApiError> {
    let app_token = app_token(app)?;
    let installations = request(state, Method::GET, "/app/installations").bearer_auth(&app_token);
    let installations: Vec<Installation> = answer(installations).await?;
    // A private app installs only on the account that owns it, so there is at most one.
    let Some(installation) = installations.into_iter().next() else {
        return Ok(None);
    };
    let path = format!("/app/installations/{}/access_tokens", installation.id);
    let mut tokens = request(state, Method::POST, &path).bearer_auth(&app_token);
    if let Some(repo) = only {
        // GitHub wants the name without its owner.
        let name = repo.rsplit('/').next();
        tokens = tokens.json(&json!({"repositories": [name], "permissions": {"contents": "read"}}));
    }
    let token: Token = answer(tokens).await?;
    Ok(Some((installation.account.login, token.token)))
}

/// A token to read the team's repositories with, or `only` one of them. GitHub ends it after an hour.
pub async fn token(
    state: &AppState,
    team_id: &str,
    only: Option<&str>,
) -> Result<String, ApiError> {
    let installed = match db::github_app(&state.pool, team_id).await? {
        Some(app) => installation(state, &app, only).await?,
        None => None,
    };
    let (_, token) = installed.ok_or_else(|| failed("the team has no installed GitHub App"))?;
    Ok(token)
}

async fn get<T: DeserializeOwned>(
    state: &AppState,
    token: &str,
    path: &str,
) -> Result<T, ApiError> {
    answer(request(state, Method::GET, path).bearer_auth(token)).await
}

pub async fn team_status(state: &AppState, team_id: &str) -> Result<Status, ApiError> {
    let Some(app) = db::github_app(&state.pool, team_id).await? else {
        return Ok(Status::default());
    };
    let installed = installation(state, &app, None).await?;
    let mut status = Status {
        app: Some(app.slug),
        ..Status::default()
    };
    let Some((account, token)) = installed else {
        return Ok(status);
    };
    status.account = Some(account);
    for page in 1.. {
        let path = format!("/installation/repositories?per_page=100&page={page}");
        let page: Page = get(state, &token, &path).await?;
        if page.repositories.is_empty() {
            break;
        }
        let names = page.repositories.into_iter().map(|repo| repo.full_name);
        status.repos.extend(names);
    }
    Ok(status)
}

#[derive(Deserialize)]
struct Branch {
    name: String,
    commit: Commit,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
}

/// Every branch of the repository with its latest commit.
pub async fn branches(
    state: &AppState,
    token: &str,
    repo: &str,
) -> Result<HashMap<String, String>, ApiError> {
    let mut heads = HashMap::new();
    for page in 1.. {
        let path = format!("/repos/{repo}/branches?per_page=100&page={page}");
        let page: Vec<Branch> = get(state, token, &path).await?;
        if page.is_empty() {
            break;
        }
        heads.extend(
            page.into_iter()
                .map(|branch| (branch.name, branch.commit.sha)),
        );
    }
    Ok(heads)
}

#[derive(Deserialize)]
struct Tree {
    tree: Vec<Entry>,
    /// GitHub cut the list short.
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct Entry {
    path: String,
    /// `blob` for a file.
    r#type: String,
    sha: String,
}

/// Every file of the commit, as its path and git's id of its content.
pub async fn files(
    state: &AppState,
    token: &str,
    repo: &str,
    commit: &str,
) -> Result<Vec<(String, String)>, ApiError> {
    let path = format!("/repos/{repo}/git/trees/{commit}?recursive=1");
    let tree: Tree = get(state, token, &path).await?;
    if tree.truncated {
        let message = "This repository has too many files to map yet";
        return Err(ApiError(StatusCode::BAD_GATEWAY, message.into()));
    }
    let files = tree.tree.into_iter().filter(|entry| entry.r#type == "blob");
    Ok(files.map(|entry| (entry.path, entry.sha)).collect())
}

/// The commit's files as a gzipped tar archive.
pub async fn archive(
    state: &AppState,
    token: &str,
    repo: &str,
    commit: &str,
) -> Result<Vec<u8>, ApiError> {
    let path = format!("/repos/{repo}/tarball/{commit}");
    let request = request(state, Method::GET, &path).bearer_auth(token);
    // The client's own limit starts again with every byte that arrives.
    let response = request.timeout(Duration::from_secs(600)).send().await;
    let mut response = response
        .and_then(reqwest::Response::error_for_status)
        .map_err(failed)?;
    let mut archive = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        archive.extend_from_slice(&chunk);
        if archive.len() > MAX_ARCHIVE {
            let message = "This repository is too large to map yet";
            return Err(ApiError(StatusCode::BAD_GATEWAY, message.into()));
        }
    }
    Ok(archive)
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
pub async fn repo_branches(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Query(of): Query<OfRepo>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    // The name goes into GitHub's address, so it must be nothing but `owner/name`.
    let plain = |part: &str| {
        part.chars()
            .all(|c| c.is_alphanumeric() || "-_.".contains(c))
    };
    if of.repo.split('/').count() != 2 || !of.repo.split('/').all(plain) {
        return Err(crate::not_found());
    }
    let token = token(&state, &team_id, None).await?;
    let repo: Repo = get(&state, &token, &format!("/repos/{}", of.repo)).await?;
    let mut names: Vec<String> = branches(&state, &token, &of.repo)
        .await?
        .into_keys()
        .collect();
    names.sort();
    Ok(Json(
        json!({"default": repo.default_branch, "branches": names}),
    ))
}

pub async fn status(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Status>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(team_status(&state, &team_id).await?))
}

#[derive(Deserialize)]
pub struct Registration {
    /// From GitHub, after the owner confirmed the app.
    code: String,
}

pub async fn register(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(registration): Json<Registration>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    let path = format!("/app-manifests/{}/conversions", registration.code);
    let app: GithubApp = answer(request(&state, Method::POST, &path)).await?;
    db::save_github_app(&state.pool, &team_id, Some(&app)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn forget(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    db::save_github_app(&state.pool, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
