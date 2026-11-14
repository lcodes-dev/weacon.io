use sqlx::{AssertSqlSafe, SqlitePool};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::failed;

pub(super) mod steps;

/// Each step brings the schema from its index to the next version; steps are only appended.
pub(super) const STEPS: &[&str] = &[
    steps::WORKSPACES,
    steps::SIGN_IN,
    steps::CHALLENGES,
    steps::ACCOUNTS,
    steps::ACCOUNT_SIGN_IN,
    steps::TASKS,
    steps::ROLES,
    steps::CHALLENGES_BY_EMAIL,
    steps::MAILED,
    steps::DOCS,
    steps::MACHINES,
    steps::DOC_BYTES,
    steps::VERSION_AGENTS,
    steps::PROPOSALS,
    steps::CHATS,
];

/// Applies the steps the database hasn't had yet, each in its own transaction that holds the
/// write lock from the start, so that two processes opening a new file don't both apply one.
pub(super) async fn migrate(writer: &SqlitePool) -> Result<(), Fault> {
    loop {
        let mut transaction = writer.begin_with("BEGIN IMMEDIATE").await.map_err(failed)?;
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

/// The steps that come before `step`.
#[cfg(test)]
pub(super) fn steps_before(step: &str) -> &'static [&'static str] {
    let at = STEPS.iter().position(|each| *each == step).unwrap();
    &STEPS[..at]
}

/// Makes a database at `path` from before `step`, holding `rows`, for tests of what that step
/// and the ones after it do to existing data.
#[cfg(test)]
pub(super) async fn database_before(path: &std::path::Path, step: &str, rows: &str) {
    use sqlx::ConnectOptions;

    let mut old = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    let before = steps_before(step);
    for step in before {
        sqlx::raw_sql(*step).execute(&mut old).await.unwrap();
    }
    let rows = format!("{rows}; PRAGMA user_version = {};", before.len());
    sqlx::raw_sql(AssertSqlSafe(rows))
        .execute(&mut old)
        .await
        .unwrap();
}

fn too_new() -> Fault {
    Fault::new(
        code!("db.too_new"),
        "The database is from a newer version of the server",
    )
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use crate::db::Db;

    #[tokio::test]
    async fn a_schema_newer_than_this_build_is_refused() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        sqlx::raw_sql("PRAGMA user_version = 1000")
            .execute(&db.writer)
            .await
            .unwrap();
        db.close().await;

        let error = Db::open(&dir.join("weacon.db")).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.too_new");
    }

    #[tokio::test]
    async fn two_openings_of_a_new_file_both_get_its_schema() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let (one, two) = tokio::join!(Db::create(&path), Db::create(&path));

        one.unwrap();
        two.unwrap();
    }

    async fn first_columns(db: &Db, sql: &'static str, table: &str) -> Vec<String> {
        sqlx::query_scalar(sql)
            .bind(table)
            .fetch_all(&db.readers)
            .await
            .unwrap()
    }

    const SERVER_WIDE: [&str; 5] = [
        "accounts",
        "sign_in_challenges",
        "sessions",
        "machines",
        "pairings",
    ];

    async fn tables(db: &Db) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&db.readers)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_persons_own_tables_hold_nothing_of_a_workspace() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let workspace = "SELECT name FROM pragma_table_info(?) WHERE name = 'workspace_id'";

        assert_eq!(
            tables(&db).await,
            [
                "workspaces",
                "members",
                "accounts",
                "sessions",
                "tasks",
                "task_versions",
                "sign_in_challenges",
                "docs",
                "doc_versions",
                "doc_pins",
                "machines",
                "pairings",
                "proposals",
                "chats",
                "chat_events",
                "chat_waiting",
                "chat_requests",
                "changes"
            ]
        );
        for table in SERVER_WIDE {
            assert!(
                first_columns(&db, workspace, table).await.is_empty(),
                "{table}"
            );
        }
    }

    #[tokio::test]
    async fn every_other_table_index_and_reference_leads_with_the_workspace() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let tables = tables(&db).await;
        let in_workspaces = |table: &&String| !SERVER_WIDE.contains(&table.as_str());
        for table in tables.iter().filter(in_workspaces) {
            let columns = first_columns(
                &db,
                "SELECT name FROM pragma_table_info(?) WHERE cid = 0",
                table,
            )
            .await;
            let indexes = first_columns(
                &db,
                "SELECT ii.name FROM pragma_index_list(?1) AS il \
                 JOIN pragma_index_info(il.name) AS ii WHERE ii.seqno = 0",
                table,
            )
            .await;
            let references = first_columns(
                &db,
                "SELECT \"from\" FROM pragma_foreign_key_list(?) \
                 WHERE seq = 0 AND \"table\" != 'accounts'",
                table,
            )
            .await;

            assert_eq!(columns, ["workspace_id"], "{table}");
            assert!(
                indexes.iter().all(|column| column == "workspace_id"),
                "{table}: {indexes:?}"
            );
            assert!(
                references.iter().all(|column| column == "workspace_id"),
                "{table}: {references:?}"
            );
        }
    }
}
