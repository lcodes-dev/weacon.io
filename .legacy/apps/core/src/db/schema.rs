use sqlx::{AssertSqlSafe, SqlitePool};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::failed;

/// The database's whole schema. Until the first release nothing is carried from one schema
/// to another: a change is made here, and a database made before it is deleted.
const SCHEMA: &str = "
CREATE TABLE map_repos (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, repo)
) STRICT;

CREATE TABLE map_layers (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    layer TEXT NOT NULL,
    commit_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, repo, layer),
    FOREIGN KEY (workspace_id, repo) REFERENCES map_repos (workspace_id, repo) ON DELETE CASCADE
) STRICT;

CREATE TABLE map_files (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    layer TEXT NOT NULL,
    path TEXT NOT NULL,
    blob TEXT,
    PRIMARY KEY (workspace_id, repo, layer, path),
    FOREIGN KEY (workspace_id, repo, layer)
        REFERENCES map_layers (workspace_id, repo, layer) ON DELETE CASCADE
) STRICT;

CREATE INDEX map_files_by_blob ON map_files (workspace_id, repo, path, blob);

CREATE TABLE map_facts (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    blob TEXT NOT NULL,
    facts TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, repo, path, blob),
    FOREIGN KEY (workspace_id, repo) REFERENCES map_repos (workspace_id, repo) ON DELETE CASCADE
) STRICT;

CREATE VIRTUAL TABLE map_search USING fts5(
    workspace_id UNINDEXED,
    repo UNINDEXED,
    layer UNINDEXED,
    path UNINDEXED,
    kind UNINDEXED,
    line UNINDEXED,
    name,
    body
);
";

/// What `PRAGMA user_version` holds beside this schema: a hash of [`SCHEMA`] (FNV-1a), so it
/// changes when the schema does. It fits the pragma and is never 0, which a new file has.
fn version() -> u32 {
    let hash = SCHEMA.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    (hash >> 1) | 1
}

/// Makes the schema in a new database, in a transaction that holds the write lock from the
/// start, so that two processes opening a new file don't both make it.
///
/// # Errors
/// `db.other_schema` for a database made with another schema.
pub(super) async fn prepare(pool: &SqlitePool) -> Result<(), Fault> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(failed)?;
    let version = version();
    let found: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
    if found == i64::from(version) {
        return Ok(());
    }
    if found != 0 {
        return Err(Fault::new(
            code!("db.other_schema"),
            "The code map's database was made by another build of the core; delete it to start again",
        ));
    }
    sqlx::raw_sql(SCHEMA)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    // A number from this file, never input.
    sqlx::raw_sql(AssertSqlSafe(format!("PRAGMA user_version = {version}")))
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    transaction.commit().await.map_err(failed)
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    #[tokio::test]
    async fn a_database_with_this_schema_opens_again_and_one_with_another_is_refused() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        prepare(&pool).await.unwrap();
        sqlx::raw_sql("INSERT INTO map_repos (workspace_id, repo) VALUES ('w', 'r')")
            .execute(&pool)
            .await
            .unwrap();
        prepare(&pool).await.unwrap();
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM map_repos")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("PRAGMA user_version = 3")
            .execute(&pool)
            .await
            .unwrap();
        let other = prepare(&pool).await.unwrap_err();

        assert_eq!(kept, 1);
        assert_eq!(other.code().as_str(), "db.other_schema");
    }
}
