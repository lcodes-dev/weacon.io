//! The core's `SQLite` database. Only this module uses the driver, and everything stored for a
//! workspace is reached through its [`Workspace`] handle.

mod map;
mod schema;

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use weacon_observability::Fault;
use weacon_protocol::{WorkspaceId, code};

pub(crate) use map::{BASE, Hit, Layer, Map, Stored};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTIONS: u32 = 4;

/// The database file `core.db` in the state folder.
#[derive(Clone, Debug)]
pub(crate) struct Db {
    pool: SqlitePool,
}

/// One workspace's view of the database: every query it runs is limited to that workspace.
#[derive(Clone, Debug)]
pub(crate) struct Workspace {
    db: Db,
    id: WorkspaceId,
}

impl Db {
    /// Opens the database in `state_dir`, creating it if it's missing, and brings its schema
    /// up to date.
    pub(crate) async fn open(state_dir: &Path) -> Result<Self, Fault> {
        let path = std::path::absolute(state_dir.join("core.db")).map_err(|error| {
            Fault::new(code!("db.path_unusable"), "The path can't be used").because(error)
        })?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(CONNECTIONS)
            .connect_with(options)
            .await
            .map_err(|error| {
                Fault::new(code!("db.open_failed"), "The database can't be opened").because(error)
            })?;
        schema::migrate(&pool).await?;
        Ok(Self { pool })
    }

    pub(crate) fn workspace(&self, id: &WorkspaceId) -> Workspace {
        Workspace {
            db: self.clone(),
            id: id.clone(),
        }
    }
}

fn failed(error: sqlx::Error) -> Fault {
    Fault::new(code!("db.failed"), "The database couldn't do that").because(error)
}

fn unreadable() -> Fault {
    Fault::new(code!("db.unreadable"), "A stored value can't be read")
}
