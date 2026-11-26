mod anthropic;
mod auth;
mod chats;
mod cores;
pub mod db;
mod docs;
mod error;
mod github;
mod group_calls;
mod http;
pub mod jobs;
mod live;
pub mod mail;
mod maps;
mod routes;
pub mod search;
pub mod secrets;
mod tasks;
mod tools;
mod writer;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

pub use maps::READER as MAP_READER;
pub use routes::router;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub url: String,
    pub github_api: String,
    pub anthropic_api: String,
    pub cores: Arc<cores::Cores>,
    pub live: live::Live,
    pub files: PathBuf,
    pub embed: search::Embed,
    pub server_key: secrets::ServerKey,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}
