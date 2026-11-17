use std::fmt::Display;
use std::sync::LazyLock;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Client, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

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

static CLIENT: LazyLock<Client> =
    LazyLock::new(|| Client::builder().user_agent("weacon").build().unwrap());

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

pub async fn team_status(state: &AppState, team_id: i64) -> Result<Status, ApiError> {
    let Some(app) = db::github_app(&state.pool, team_id).await? else {
        return Ok(Status::default());
    };
    let app_token = app_token(&app)?;
    let mut status = Status {
        app: Some(app.slug),
        ..Status::default()
    };
    let installations = request(state, Method::GET, "/app/installations").bearer_auth(&app_token);
    let installations: Vec<Installation> = answer(installations).await?;
    // A private app installs only on the account that owns it, so there is at most one.
    let Some(installation) = installations.into_iter().next() else {
        return Ok(status);
    };
    status.account = Some(installation.account.login);

    let path = format!("/app/installations/{}/access_tokens", installation.id);
    let token: Token = answer(request(state, Method::POST, &path).bearer_auth(&app_token)).await?;
    for page in 1.. {
        let path = format!("/installation/repositories?per_page=100&page={page}");
        let page: Page =
            answer(request(state, Method::GET, &path).bearer_auth(&token.token)).await?;
        if page.repositories.is_empty() {
            break;
        }
        let names = page.repositories.into_iter().map(|repo| repo.full_name);
        status.repos.extend(names);
    }
    Ok(status)
}

pub async fn status(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
) -> Result<Json<Status>, ApiError> {
    membership(&user, team_id)?;
    Ok(Json(team_status(&state, team_id).await?))
}

#[derive(Deserialize)]
pub struct Registration {
    /// From GitHub, after the owner confirmed the app.
    code: String,
}

pub async fn register(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
    Json(registration): Json<Registration>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    let path = format!("/app-manifests/{}/conversions", registration.code);
    let app: GithubApp = answer(request(&state, Method::POST, &path)).await?;
    db::save_github_app(&state.pool, team_id, Some(&app)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn forget(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    db::save_github_app(&state.pool, team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
