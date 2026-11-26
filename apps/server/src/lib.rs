mod auth;
mod chats;
mod config;
pub mod db;
mod docs;
mod github;
mod groups;
mod live;
mod machines;
pub mod mail;
pub mod map;
mod pages;
mod projects;
pub mod search;
pub mod secrets;
mod tasks;
mod teams;
mod tools;
mod weacon;
mod web;
mod writer;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::sync::Notify;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    /// The address people open in their browser, for links in mail.
    pub url: String,
    /// `https://api.github.com`, or a fake one in tests.
    pub github_api: String,
    /// Wakes the map's worker before its minute is up.
    pub map_wake: Arc<Notify>,
    /// `https://api.anthropic.com`, or a fake one in tests.
    pub anthropic_api: String,
    /// The cores of people's machines that are connected now.
    pub cores: Arc<machines::Cores>,
    /// What tells the browsers that something changed.
    pub live: live::Live,
    /// The groups the team's own agent is answering in now.
    pub weacon: Arc<Mutex<HashSet<String>>>,
    /// The folder that holds the files uploaded for docs.
    pub files: PathBuf,
    /// What embeds the docs for searching: `doc_search::embed`, or a stand-in in tests.
    pub embed: search::Embed,
    /// Seals the secrets the database holds.
    pub server_key: secrets::ServerKey,
}

/// Seconds since 1970.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

pub fn router(state: AppState) -> Router {
    let log = TraceLayer::new_for_http()
        .make_span_with(|request: &Request| {
            // The route's pattern, because an address can hold an invite's token.
            let route = request.extensions().get::<MatchedPath>();
            let route = route.map_or("", MatchedPath::as_str);
            tracing::info_span!("request", method = %request.method(), route)
        })
        .on_response(DefaultOnResponse::new().level(Level::INFO));
    let api = Router::new()
        .route("/session", get(auth::session))
        .route("/sign-in/code", post(auth::request_code))
        .route("/sign-in/verify", post(auth::verify))
        .route("/sign-out", post(auth::sign_out))
        .route(
            "/config/mail",
            get(config::mail)
                .put(config::save_mail)
                .delete(config::clear_mail),
        )
        .route("/config/mail/test", post(config::test_mail))
        .route("/teams/{id}", get(teams::team).put(teams::rename))
        .route("/teams/{id}/invites", post(teams::invite))
        .route("/teams/{id}/invites/{invite_id}", delete(teams::revoke))
        .route(
            "/teams/{id}/members/{user_id}",
            delete(teams::remove_member),
        )
        .route(
            "/teams/{id}/projects",
            get(projects::list).post(projects::create),
        )
        .route(
            "/teams/{id}/projects/{project_id}",
            put(projects::update).delete(projects::delete),
        )
        .route("/teams/{id}/projects/{project_id}/map", get(map::show))
        .route(
            "/teams/{id}/projects/{project_id}/docs",
            get(docs::show).post(docs::start),
        )
        .route(
            "/teams/{id}/projects/{project_id}/docs/pages",
            post(pages::add).put(pages::order),
        )
        .route(
            "/teams/{id}/projects/{project_id}/docs/pages/{page_id}",
            put(pages::save).delete(pages::remove),
        )
        .route(
            "/teams/{id}/projects/{project_id}/docs/pages/{page_id}/versions",
            get(pages::versions),
        )
        .route(
            "/teams/{id}/projects/{project_id}/docs/pages/{page_id}/notes/{index}",
            delete(pages::dismiss),
        )
        .route(
            "/teams/{id}/projects/{project_id}/docs/requests",
            post(docs::ask),
        )
        .route(
            "/teams/{id}/projects/{project_id}/docs/proposals/{proposal_id}",
            post(pages::accept).delete(pages::reject),
        )
        .route(
            "/teams/{id}/projects/{project_id}/files",
            // A little over the limit, so that a too large file gets our own message.
            post(pages::upload).layer(DefaultBodyLimit::max(pages::FILE_BYTES + 1024)),
        )
        .route(
            "/teams/{id}/projects/{project_id}/files/{file_id}",
            get(pages::file),
        )
        .route("/shares/{token}/files/{file_id}", get(pages::shared_file))
        .route(
            "/teams/{id}/projects/{project_id}/shares",
            get(docs::shares).post(docs::share),
        )
        .route(
            "/teams/{id}/projects/{project_id}/shares/{token}",
            delete(docs::end_share),
        )
        .route("/shares/{token}", get(docs::shared))
        .route(
            "/teams/{id}/projects/{project_id}/tasks",
            get(tasks::list).post(tasks::create).put(tasks::order),
        )
        .route(
            "/teams/{id}/projects/{project_id}/tasks/{task_id}",
            put(tasks::change).delete(tasks::delete),
        )
        .route(
            "/teams/{id}/projects/{project_id}/tasks/{task_id}/docs",
            get(tasks::docs),
        )
        .route("/tools", post(tools::call))
        .route("/teams/{id}/groups", get(groups::list).post(groups::create))
        .route(
            "/teams/{id}/groups/{group_id}",
            get(groups::show).put(groups::change),
        )
        .route("/teams/{id}/groups/{group_id}/members", post(groups::add))
        .route(
            "/teams/{id}/groups/{group_id}/members/me",
            delete(groups::leave),
        )
        .route(
            "/teams/{id}/groups/{group_id}/messages",
            post(groups::write),
        )
        .route("/teams/{id}/chats", get(chats::list))
        .route(
            "/teams/{id}/projects/{project_id}/chats",
            post(chats::start),
        )
        .route(
            "/teams/{id}/chats/{chat_id}",
            get(chats::show).delete(chats::delete),
        )
        .route("/teams/{id}/chats/{chat_id}/messages", post(chats::say))
        .route("/teams/{id}/chats/{chat_id}/stop", post(chats::stop))
        .route("/teams/{id}/chats/{chat_id}/diff", get(chats::diff))
        .route("/teams/{id}/chats/{chat_id}/push", post(chats::push))
        .route("/teams/{id}/chats/{chat_id}/hosts", post(chats::allow_host))
        .route(
            "/teams/{id}/chats/{chat_id}/hosts/{host}",
            delete(chats::forget_host),
        )
        .route("/live", get(live::socket))
        .route("/machines", get(machines::list).post(machines::pair))
        .route("/machines/codes", post(machines::code))
        .route("/machines/connect", get(machines::connect))
        .route("/machines/{id}", delete(machines::remove))
        .route("/machines/{id}/sign-in/{agent}", get(machines::sign_in))
        .route(
            "/teams/{id}/writer",
            put(writer::save).delete(writer::forget),
        )
        .route(
            "/teams/{id}/key",
            put(writer::save_key).delete(writer::forget_key),
        )
        .route("/teams/{id}/github/branches", get(github::repo_branches))
        .route(
            "/teams/{id}/github",
            get(github::status)
                .post(github::register)
                .delete(github::forget),
        )
        .route("/invites/{token}", get(teams::invitation))
        .route("/invites/{token}/accept", post(teams::accept))
        .fallback(async || not_found())
        .layer(log)
        // Added after the layer, so health checks aren't logged.
        .route("/health", get(health))
        .with_state(state);
    Router::new().nest("/api", api).fallback(web::serve)
}

async fn health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query_scalar!("SELECT 1")
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(json!({"status": "ok"})))
}

#[derive(Debug)]
pub struct ApiError(StatusCode, String);

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!("database error: {error}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong".into(),
        )
    }
}
