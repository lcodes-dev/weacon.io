pub mod chats;
pub mod docs;
pub mod files;
pub mod groups;
pub mod jobs;
pub mod machines;
pub mod maps;
pub mod projects;
pub mod proposals;
pub mod search;
pub mod settings;
pub mod shares;
pub mod tasks;
pub mod teams;
pub mod users;

use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub async fn open(path: &str) -> sqlx::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    #[cfg(unix)]
    if let Err(error) = keep_private(path) {
        tracing::warn!("{path} could not be made private: {error}");
    }
    migrated(SqlitePool::connect_with(options).await?).await
}

pub async fn memory() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    migrated(pool).await.unwrap()
}

pub async fn ping(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query_scalar!("SELECT 1").fetch_one(pool).await?;
    Ok(())
}

async fn migrated(pool: SqlitePool) -> sqlx::Result<SqlitePool> {
    sqlx::migrate!()
        .run(&pool)
        .await
        .map_err(|error| sqlx::Error::Migrate(Box::new(error)))?;
    Ok(pool)
}

#[cfg(unix)]
fn keep_private(path: &str) -> std::io::Result<()> {
    use std::fs::{OpenOptions, Permissions, set_permissions};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    for file in [
        path.to_owned(),
        format!("{path}-wal"),
        format!("{path}-shm"),
    ] {
        if std::fs::exists(&file)? {
            set_permissions(&file, Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn as_json<S: serde::Serializer>(json: &str, to: S) -> Result<S::Ok, S::Error> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(serde::ser::Error::custom)?;
    value.serialize(to)
}

fn as_optional_json<S: serde::Serializer>(json: &Option<String>, to: S) -> Result<S::Ok, S::Error> {
    as_json(json.as_deref().unwrap_or("null"), to)
}
