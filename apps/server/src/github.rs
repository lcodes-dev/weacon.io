use std::collections::HashMap;
use std::fmt::Display;
use std::sync::LazyLock;
use std::time::Duration;

use axum::http::StatusCode;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Client, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{ApiError, Result};
use crate::{AppState, db, now};

#[derive(Deserialize)]
pub struct GithubApp {
    pub id: i64,
    pub slug: String,
    pub pem: String,
}

#[derive(Default, Serialize)]
pub struct Status {
    app: Option<String>,
    account: Option<String>,
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

const MAX_ARCHIVE: usize = 200_000_000;

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent("weacon")
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .build()
        .unwrap()
});

pub fn request(state: &AppState, method: Method, path: &str) -> RequestBuilder {
    CLIENT.request(method, format!("{}{path}", state.github_api))
}

fn failed(error: impl Display) -> ApiError {
    tracing::error!("GitHub call failed: {error}");
    ApiError::bad_gateway("GitHub refused. Check that the team's GitHub App still exists.")
}

pub async fn answer<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    let answer = request.send().await.map_err(failed)?;
    if !answer.status().is_success() {
        let (status, url) = (answer.status(), answer.url().clone());
        let said = answer.text().await.unwrap_or_default();
        return Err(failed(format!("{status} for {url}: {said}")));
    }
    answer.json().await.map_err(failed)
}

fn app_token(app: &GithubApp) -> Result<String> {
    // GitHub asks for a start in the past, against clock drift, and at most 10 minutes.
    let claims = json!({"iss": app.id.to_string(), "iat": now() - 60, "exp": now() + 300});
    EncodingKey::from_rsa_pem(app.pem.as_bytes())
        .and_then(|key| jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key))
        .map_err(failed)
}

async fn installation(
    state: &AppState,
    app: &GithubApp,
    only: Option<&str>,
    may: Value,
) -> Result<Option<(String, String)>> {
    let app_token = app_token(app)?;
    let installations = request(state, Method::GET, "/app/installations").bearer_auth(&app_token);
    let installations: Vec<Installation> = answer(installations).await?;
    let Some(installation) = installations.into_iter().next() else {
        return Ok(None);
    };
    let mut scope = json!({"permissions": may});
    if let Some(repo) = only {
        scope["repositories"] = json!([repo.rsplit('/').next()]);
    }
    let path = format!("/app/installations/{}/access_tokens", installation.id);
    let tokens = request(state, Method::POST, &path).bearer_auth(&app_token);
    let token = tokens.json(&scope).send().await.map_err(failed)?;
    if token.status() == StatusCode::UNPROCESSABLE_ENTITY {
        let said = token.text().await.unwrap_or_default();
        tracing::error!("GitHub gave no token for {scope}: {said}");
        let message = "The team's GitHub App may only read. Its owner has to let it write \
            contents and pull requests, in the app's settings on GitHub, and accept the \
            change where the app is installed.";
        return Err(ApiError::invalid(message));
    }
    let token = async { token.error_for_status()?.json::<Token>().await };
    let token = token.await.map_err(failed)?;
    Ok(Some((installation.account.login, token.token)))
}

pub fn reading() -> Value {
    json!({"contents": "read"})
}

pub async fn token(
    state: &AppState,
    team_id: &str,
    only: Option<&str>,
    may: Value,
) -> Result<String> {
    let installed = match db::settings::github_app(&state.pool, &state.server_key, team_id).await? {
        Some(app) => installation(state, &app, only, may).await?,
        None => None,
    };
    let (_, token) = installed.ok_or_else(|| failed("the team has no installed GitHub App"))?;
    Ok(token)
}

#[derive(Deserialize)]
pub struct Pull {
    pub number: i64,
    pub html_url: String,
}

pub async fn open_pull(
    state: &AppState,
    token: &str,
    repo: &str,
    (head, base): (&str, &str),
    (title, body): (&str, &str),
) -> Result<Pull> {
    let path = format!("/repos/{repo}/pulls");
    let pull = |draft: bool| {
        let pull =
            json!({"title": title, "body": body, "head": head, "base": base, "draft": draft});
        answer(
            request(state, Method::POST, &path)
                .bearer_auth(token)
                .json(&pull),
        )
    };
    match pull(true).await {
        Ok(pull) => Ok(pull),
        Err(_) => pull(false).await,
    }
}

pub async fn get<T: DeserializeOwned>(state: &AppState, token: &str, path: &str) -> Result<T> {
    answer(request(state, Method::GET, path).bearer_auth(token)).await
}

pub async fn team_status(state: &AppState, team_id: &str) -> Result<Status> {
    let Some(app) = db::settings::github_app(&state.pool, &state.server_key, team_id).await? else {
        return Ok(Status::default());
    };
    let installed = installation(state, &app, None, reading()).await?;
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

pub async fn branches(
    state: &AppState,
    token: &str,
    repo: &str,
) -> Result<HashMap<String, String>> {
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
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct Entry {
    path: String,
    r#type: String,
    sha: String,
}

pub async fn files(
    state: &AppState,
    token: &str,
    repo: &str,
    commit: &str,
) -> Result<Vec<(String, String)>> {
    let path = format!("/repos/{repo}/git/trees/{commit}?recursive=1");
    let tree: Tree = get(state, token, &path).await?;
    if tree.truncated {
        return Err(ApiError::bad_gateway(
            "This repository has too many files to map yet",
        ));
    }
    let files = tree.tree.into_iter().filter(|entry| entry.r#type == "blob");
    Ok(files.map(|entry| (entry.path, entry.sha)).collect())
}

pub async fn archive(state: &AppState, token: &str, repo: &str, commit: &str) -> Result<Vec<u8>> {
    let path = format!("/repos/{repo}/tarball/{commit}");
    let request = request(state, Method::GET, &path).bearer_auth(token);
    let response = request.timeout(Duration::from_secs(600)).send().await;
    let mut response = response
        .and_then(reqwest::Response::error_for_status)
        .map_err(failed)?;
    let mut archive = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        archive.extend_from_slice(&chunk);
        if archive.len() > MAX_ARCHIVE {
            return Err(ApiError::bad_gateway(
                "This repository is too large to map yet",
            ));
        }
    }
    Ok(archive)
}
