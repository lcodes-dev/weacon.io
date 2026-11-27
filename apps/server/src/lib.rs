pub mod accounts;
pub mod anthropic;
pub mod auth;
pub mod chats;
pub mod db;
pub mod docs;
pub mod error;
pub mod groups;
pub mod jobs;
pub mod live;
pub mod machines;
pub mod projects;
mod routes;
pub mod settings;
pub mod tasks;
pub mod web;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub use projects::maps::READER as MAP_READER;
pub use routes::router;
use sqlx::SqlitePool;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub url: String,
    pub github_api: String,
    pub anthropic_api: String,
    pub cores: Arc<machines::Cores>,
    pub live: live::Live,
    pub files: PathBuf,
    pub embed: docs::search::Embed,
    pub server_key: settings::secrets::ServerKey,
    pub retry_wait: std::time::Duration,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}
