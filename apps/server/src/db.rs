//! The server's `SQLite` database. Only this module uses the driver, and every query
//! about a workspace goes through its [`Workspace`] handle.

mod account;
mod backup;
mod changes;
mod chats;
mod docs;
mod machine;
mod owned;
mod schema;
mod tasks;
mod workspace;

#[cfg(test)]
pub(crate) use chats::test_support;
pub(crate) use chats::{
    CHAT_QUOTA, Chat, Incoming, NewChat, NewRequest, Recorded, chat_not_found, chat_quota_full,
};
pub(crate) use tasks::task_not_found;

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteSynchronous};
use weacon_model::Revision;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, Author, MemberId, WorkspaceId, code};

#[cfg(test)]
pub(crate) use account::CODE_LIFETIME;
pub(crate) use account::{Account, Attempt};
pub(crate) use backup::restore;
pub(crate) use machine::{Claim, Confirmation, NewPairing};
pub(crate) use owned::NewWorkspace;
pub(crate) use workspace::Workspace;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const READERS: u32 = 8;
const WAL_TRIES: u32 = 50;
const WAL_RETRY: Duration = Duration::from_millis(100);

/// One database file in WAL mode: a single writer connection, and readers beside it.
#[derive(Clone, Debug)]
pub(crate) struct Db {
    writer: SqlitePool,
    readers: SqlitePool,
}

impl Db {
    /// Opens the database at `path`, creating it if it's missing, and brings its schema up
    /// to date.
    pub(crate) async fn create(path: &Path) -> Result<Self, Fault> {
        Self::connect(path, true).await
    }

    /// Opens the database at `path`, which must exist, and brings its schema up to date.
    pub(crate) async fn open(path: &Path) -> Result<Self, Fault> {
        Self::connect(path, false).await
    }

    async fn connect(path: &Path, create: bool) -> Result<Self, Fault> {
        let path = file_path(path)?;
        if !create && !path.exists() {
            return Err(not_found());
        }
        let options = options(&path);
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone().create_if_missing(create))
            .await
            .map_err(open_failed)?;
        use_wal(&mut *writer.acquire().await.map_err(open_failed)?).await?;
        schema::migrate(&writer).await?;
        let readers = SqlitePoolOptions::new()
            .max_connections(READERS)
            .connect_with(options.read_only(true))
            .await
            .map_err(open_failed)?;
        Ok(Self { writer, readers })
    }

    /// Creates a workspace with a new `id`.
    pub(crate) async fn create_workspace(
        &self,
        id: &WorkspaceId,
        name: &str,
    ) -> Result<Workspace, Fault> {
        let mut writer = self.writer.acquire().await.map_err(failed)?;
        insert_workspace(&mut writer, id, name).await?;
        Ok(Workspace::new(self.clone(), id.clone()))
    }

    /// The workspace `id`; one there isn't is `workspace.not_found`.
    pub(crate) async fn workspace(&self, id: &WorkspaceId) -> Result<Workspace, Fault> {
        self.find_workspace(id)
            .await?
            .ok_or_else(|| Fault::new(code!("workspace.not_found"), "There is no such workspace"))
    }

    /// The workspace `id` and who is asking, if the token hashed as `token_hash` has a live
    /// session whose account is a member there.
    pub(crate) async fn member_workspace(
        &self,
        id: &WorkspaceId,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<(Workspace, MemberId)>, Fault> {
        let workspace = Workspace::new(self.clone(), id.clone());
        let member = workspace.session_member(token_hash, now).await?;
        Ok(member.map(|member| (workspace, member)))
    }

    /// The workspace `id`, if there is one.
    pub(crate) async fn find_workspace(
        &self,
        id: &WorkspaceId,
    ) -> Result<Option<Workspace>, Fault> {
        let found = sqlx::query("SELECT 1 FROM workspaces WHERE workspace_id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.readers)
            .await
            .map_err(failed)?;
        Ok(found.map(|_| Workspace::new(self.clone(), id.clone())))
    }

    /// The handle to the account `id`, which reaches only that account's own rows; the id
    /// comes from a session, so the account is there.
    pub(crate) fn account(&self, id: AccountId) -> Account {
        Account {
            db: self.clone(),
            id,
        }
    }

    /// Closes every connection, so the file can be copied or removed.
    pub(crate) async fn close(&self) {
        self.readers.close().await;
        self.writer.close().await;
    }
}

async fn insert_workspace(
    writer: &mut SqliteConnection,
    id: &WorkspaceId,
    name: &str,
) -> Result<(), Fault> {
    sqlx::query("INSERT INTO workspaces (workspace_id, name) VALUES (?, ?)")
        .bind(id.as_str())
        .bind(name)
        .execute(writer)
        .await
        .map_err(failed)?;
    Ok(())
}

/// `path` made absolute, so that `SQLite` never reads it as a `file:` URI.
fn file_path(path: &Path) -> Result<PathBuf, Fault> {
    std::path::absolute(path).map_err(|error| path_unusable().because(error))
}

fn path_unusable() -> Fault {
    Fault::new(code!("db.path_unusable"), "The path can't be used")
}

/// A value read back from the database, such as an id, checked like one that arrived.
fn stored<T: TryFrom<String>>(value: String) -> Result<T, Fault> {
    T::try_from(value).map_err(|_| unreadable())
}

fn stored_version(version: i64) -> Result<Revision, Fault> {
    u64::try_from(version)
        .map(Revision::new)
        .map_err(|_| unreadable())
}

/// How `author` is stored, unless it is one from a newer peer.
fn author_columns(author: &Author) -> Option<(&'static str, &MemberId)> {
    match author {
        Author::Person { member } => Some(("person", member)),
        Author::Agent { owner } => Some(("agent", owner)),
        Author::Newer => None,
    }
}

fn stored_author(kind: &str, member: String) -> Result<Author, Fault> {
    let member = stored(member)?;
    match kind {
        "person" => Ok(Author::Person { member }),
        "agent" => Ok(Author::Agent { owner: member }),
        _ => Err(unreadable()),
    }
}

fn unreadable() -> Fault {
    Fault::new(code!("db.unreadable"), "A stored value can't be read")
}

fn not_found() -> Fault {
    Fault::new(code!("db.not_found"), "There is no database there")
}

/// Switches the file to WAL mode, which lasts. The switch needs a lock that the busy timeout
/// doesn't wait for, so it is tried again while another process holds the file.
async fn use_wal(writer: &mut SqliteConnection) -> Result<(), Fault> {
    for _ in 0..WAL_TRIES {
        let switched = sqlx::query_scalar::<_, String>("PRAGMA journal_mode = WAL")
            .fetch_one(&mut *writer)
            .await;
        match switched {
            Ok(mode) if mode == "wal" => return Ok(()),
            Err(error) if is_busy(&error) => tokio::time::sleep(WAL_RETRY).await,
            Ok(_) => return Err(open_fault()),
            Err(error) => return Err(open_failed(error)),
        }
    }
    Err(Fault::new(
        code!("db.busy"),
        "Another process is holding the database",
    ))
}

/// Whether `error` is `SQLITE_BUSY` or one of its extended codes, which share its low byte.
fn is_busy(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .and_then(|code| code.parse::<u32>().ok())
        .is_some_and(|code| code & 0xff == 5)
}

fn options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true)
}

fn open_failed(error: sqlx::Error) -> Fault {
    open_fault().because(error)
}

fn open_fault() -> Fault {
    Fault::new(code!("db.open_failed"), "The database couldn't be opened")
}

/// A failed query; one that would duplicate a unique value is `db.exists`.
fn failed(error: sqlx::Error) -> Fault {
    let exists = error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation);
    if exists {
        return self::exists().because(error);
    }
    Fault::new(code!("db.failed"), "The database failed").because(error)
}

fn exists() -> Fault {
    Fault::new(code!("db.exists"), "It already exists")
}

#[cfg(test)]
mod tests {
    use sqlx::{ConnectOptions, Row};
    use tokio::time::Instant;
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::workspace_id;

    async fn pragma(db: &Db, name: &str) -> String {
        let row = sqlx::raw_sql(sqlx::AssertSqlSafe(format!("PRAGMA {name}")))
            .fetch_one(&db.writer)
            .await
            .unwrap();
        row.try_get::<i64, _>(0)
            .map(|number| number.to_string())
            .or_else(|_| row.try_get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn opens_one_writer_in_wal_mode_that_waits_when_busy() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();

        assert_eq!(pragma(&db, "journal_mode").await, "wal");
        assert_eq!(pragma(&db, "busy_timeout").await, "5000");
        assert_eq!(pragma(&db, "foreign_keys").await, "1");
        assert_eq!(pragma(&db, "synchronous").await, "1");
        assert_eq!(db.writer.options().get_max_connections(), 1);
        assert_eq!(db.readers.options().get_max_connections(), READERS);
    }

    #[tokio::test]
    async fn readers_cannot_write() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let error = sqlx::query("INSERT INTO workspaces (workspace_id, name) VALUES ('w', 'n')")
            .execute(&db.readers)
            .await
            .unwrap_err();

        assert_eq!(failed(error).code().as_str(), "db.failed");
    }

    #[tokio::test]
    async fn reopening_keeps_the_data_and_the_schema() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        db.create_workspace(&workspace_id("w1"), "Team")
            .await
            .unwrap();
        db.close().await;
        let again = Db::open(&dir.join("weacon.db")).await.unwrap();

        again.workspace(&workspace_id("w1")).await.unwrap();
        assert_eq!(
            pragma(&again, "user_version").await,
            schema::STEPS.len().to_string()
        );
    }

    #[tokio::test]
    async fn closing_closes_every_connection() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        db.close().await;

        assert!(db.writer.is_closed());
        assert!(db.readers.is_closed());
    }

    // A database that another connection is about to write to, so it can't change its mode.
    async fn held(dir: &TempDir) -> (SqliteConnection, SqliteConnection) {
        let path = dir.join("weacon.db");
        let mut holder = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::raw_sql("BEGIN IMMEDIATE; CREATE TABLE held (x INTEGER)")
            .execute(&mut holder)
            .await
            .unwrap();
        let writer = options(&path).connect().await.unwrap();
        (holder, writer)
    }

    #[tokio::test(start_paused = true)]
    async fn the_switch_to_wal_waits_out_another_writer() {
        let dir = TempDir::new().unwrap();
        let (mut holder, mut writer) = held(&dir).await;
        let switching = tokio::spawn(async move { use_wal(&mut writer).await });
        tokio::time::sleep(WAL_RETRY * 3).await;
        sqlx::raw_sql("COMMIT").execute(&mut holder).await.unwrap();

        switching.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_switch_to_wal_gives_up_after_a_while() {
        let dir = TempDir::new().unwrap();
        let (_holder, mut writer) = held(&dir).await;
        let started = Instant::now();
        let error = use_wal(&mut writer).await.unwrap_err();

        assert_eq!(error.code().as_str(), "db.busy");
        assert_eq!(started.elapsed(), WAL_RETRY * WAL_TRIES);
    }

    #[tokio::test(start_paused = true)]
    async fn other_errors_are_not_tried_again() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        drop(
            options(&path)
                .create_if_missing(true)
                .connect()
                .await
                .unwrap(),
        );
        let mut read_only = options(&path).read_only(true).connect().await.unwrap();
        let started = Instant::now();
        let error = use_wal(&mut read_only).await.unwrap_err();

        assert_eq!(error.code().as_str(), "db.open_failed");
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test]
    async fn a_database_that_cant_use_wal_is_refused() {
        let mut memory = SqliteConnectOptions::new().connect().await.unwrap();
        let error = use_wal(&mut memory).await.unwrap_err();

        assert_eq!(error.code().as_str(), "db.open_failed");
    }

    #[tokio::test]
    async fn only_creating_makes_a_missing_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");

        let error = Db::open(&path).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.not_found");
        assert!(!path.exists());
        Db::create(&path).await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_file_that_isnt_a_database_fails_to_open() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.join("weacon.db"),
            "MARKER not a database, but long enough to have a header",
        )
        .unwrap();

        let error = Db::create(&dir.join("weacon.db")).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.open_failed");
    }
}
