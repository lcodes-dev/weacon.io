//! The Weacon server: one program that keeps the data in SQLite, answers the web app's
//! requests and talks to the core on each person's machine.

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
    /// The address people open in their browser, for links in mail.
    pub url: String,
    /// `https://api.github.com`, or a fake one in tests.
    pub github_api: String,
    /// `https://api.anthropic.com`, or a fake one in tests.
    pub anthropic_api: String,
    /// The cores of people's machines that are connected now.
    pub cores: Arc<cores::Cores>,
    pub live: live::Live,
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
