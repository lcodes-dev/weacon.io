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

pub(crate) use map::{BASE, Hit, Layer, LocalRepo, Looking, Map, Stored};

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
    /// Opens the database in `state_dir`, making it with its schema if it's missing.
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
        connect(
            SqlitePoolOptions::new().max_connections(CONNECTIONS),
            options,
        )
        .await
    }

    /// A database in memory, gone once dropped, with the same schema.
    pub(crate) async fn memory() -> Result<Self, Fault> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        // Each connection to memory is a database of its own, so there is one, kept open.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None);
        connect(pool, options).await
    }

    pub(crate) fn workspace(&self, id: &WorkspaceId) -> Workspace {
        Workspace {
            db: self.clone(),
            id: id.clone(),
        }
    }
}

/// A pool of `pool`'s connections with `options`, its schema made if the database is new.
async fn connect(pool: SqlitePoolOptions, options: SqliteConnectOptions) -> Result<Db, Fault> {
    let pool = pool.connect_with(options).await.map_err(|error| {
        Fault::new(code!("db.open_failed"), "The database can't be opened").because(error)
    })?;
    schema::prepare(&pool).await?;
    Ok(Db { pool })
}

fn failed(error: sqlx::Error) -> Fault {
    Fault::new(code!("db.failed"), "The database couldn't do that").because(error)
}
