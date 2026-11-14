use sqlx::{AssertSqlSafe, SqlitePool};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::failed;

/// Each step brings the schema from its index to the next version; steps are only appended.
const STEPS: &[&str] = &["
CREATE TABLE map_repos (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    PRIMARY KEY (workspace_id, repo)
) STRICT;

CREATE TABLE map_layers (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    layer TEXT NOT NULL,
    commit_id TEXT NOT NULL,
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

CREATE TABLE map_facts (
    workspace_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    blob TEXT NOT NULL,
    facts TEXT NOT NULL,
    PRIMARY KEY (workspace_id, repo, path, blob),
    FOREIGN KEY (workspace_id, repo) REFERENCES map_repos (workspace_id, repo) ON DELETE CASCADE
) STRICT;

CREATE VIRTUAL TABLE map_search USING fts5(
    workspace_id UNINDEXED,
    repo UNINDEXED,
    layer UNINDEXED,
    path UNINDEXED,
    kind UNINDEXED,
    name,
    body
);
"];

pub(super) async fn migrate(pool: &SqlitePool) -> Result<(), Fault> {
    loop {
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(failed)?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *transaction)
            .await
            .map_err(failed)?;
        let done = usize::try_from(version).map_err(|_| too_new())?;
        let Some(step) = STEPS.get(done) else {
            return if done == STEPS.len() {
                Ok(())
            } else {
                Err(too_new())
            };
        };
        sqlx::raw_sql(*step)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        // A number from this file, never input.
        sqlx::raw_sql(AssertSqlSafe(format!("PRAGMA user_version = {}", done + 1)))
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        transaction.commit().await.map_err(failed)?;
    }
}

fn too_new() -> Fault {
    Fault::new(code!("db.too_new"), "The database is from a newer Weacon")
}
