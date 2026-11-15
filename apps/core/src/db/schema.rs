use sqlx::{AssertSqlSafe, SqlitePool};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::failed;

/// Each step brings the schema from its index to the next version; steps are only appended.
const STEPS: &[&str] = &[
    "
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
",
    "
DROP TABLE map_search;

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

-- Each layer is read again, so what search finds is stored with its line.
DELETE FROM map_layers;
",
    "
-- Facts kept before versions count as older than every reader's.
ALTER TABLE map_facts ADD COLUMN version INTEGER NOT NULL DEFAULT 0;
",
];

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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    #[tokio::test]
    async fn a_map_stored_before_lines_and_versions_is_read_again() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(STEPS[0]).execute(&pool).await.unwrap();
        sqlx::raw_sql(
            "PRAGMA user_version = 1;
             INSERT INTO map_repos (workspace_id, repo) VALUES ('w', 'r');
             INSERT INTO map_layers VALUES ('w', 'r', 'base', 'c0ffee', 1);
             INSERT INTO map_facts VALUES ('w', 'r', 'src/lib.rs', 'b1', 'null');",
        )
        .execute(&pool)
        .await
        .unwrap();

        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();

        let count = |sql| sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool);
        assert_eq!(count("SELECT count(*) FROM map_layers").await.unwrap(), 0);
        assert_eq!(count("SELECT count(*) FROM map_repos").await.unwrap(), 1);
        assert_eq!(count("PRAGMA user_version").await.unwrap(), 3);
        let older = count("SELECT count(*) FROM map_facts WHERE version = 0").await;
        assert_eq!(older.unwrap(), 1);
        let lines = count("SELECT count(line) FROM map_search").await;
        assert_eq!(lines.unwrap(), 0);
    }
}
