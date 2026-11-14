//! The rows a layer's replacement writes, inside its transaction.

use sqlx::{QueryBuilder, Sqlite};
use weacon_codemap::{Entry, Facts};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::super::failed;
use super::Stored;

/// How many rows one insert carries, well within `SQLite`'s limit on bound values.
pub(super) const BATCH: usize = 100;

pub(super) type Transaction<'a> = sqlx::Transaction<'a, Sqlite>;

pub(super) async fn insert_files(
    transaction: &mut Transaction<'_>,
    (workspace, repo, layer): (&str, &str, &str),
    files: &[Stored],
) -> Result<(), Fault> {
    for batch in files.chunks(BATCH) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO map_files (workspace_id, repo, layer, path, blob) ",
        );
        query.push_values(batch, |mut row, file| {
            row.push_bind(workspace)
                .push_bind(repo)
                .push_bind(layer)
                .push_bind(&file.path)
                .push_bind(&file.blob);
        });
        query
            .build()
            .execute(&mut **transaction)
            .await
            .map_err(failed)?;
    }
    Ok(())
}

pub(super) async fn add_repo(
    transaction: &mut Transaction<'_>,
    (workspace, repo): (&str, &str),
) -> Result<(), Fault> {
    sqlx::query("INSERT OR IGNORE INTO map_repos (workspace_id, repo) VALUES (?, ?)")
        .bind(workspace)
        .bind(repo)
        .execute(&mut **transaction)
        .await
        .map_err(failed)?;
    Ok(())
}

/// Removes the facts no layer's file has any more.
pub(super) async fn drop_unused_facts(
    transaction: &mut Transaction<'_>,
    (workspace, repo): (&str, &str),
) -> Result<(), Fault> {
    sqlx::query(
        "DELETE FROM map_facts WHERE workspace_id = ?1 AND repo = ?2 AND NOT EXISTS \
         (SELECT 1 FROM map_files WHERE workspace_id = ?1 AND repo = ?2 \
          AND path = map_facts.path AND blob = map_facts.blob)",
    )
    .bind(workspace)
    .bind(repo)
    .execute(&mut **transaction)
    .await
    .map_err(failed)?;
    Ok(())
}

/// Whether the stored layer was read by a refresh after `generation`.
pub(super) async fn newer_than(
    transaction: &mut Transaction<'_>,
    (workspace, repo, layer): (&str, &str, &str),
    generation: i64,
) -> Result<bool, Fault> {
    let stored: Option<i64> = sqlx::query_scalar(
        "SELECT generation FROM map_layers WHERE workspace_id = ? AND repo = ? AND layer = ?",
    )
    .bind(workspace)
    .bind(repo)
    .bind(layer)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(failed)?;
    Ok(stored.is_some_and(|stored| stored > generation))
}

pub(super) async fn insert_facts(
    transaction: &mut Transaction<'_>,
    (workspace, repo): (&str, &str),
    facts: &[(String, String, Option<Facts>)],
) -> Result<(), Fault> {
    for batch in facts.chunks(BATCH) {
        let rows = batch
            .iter()
            .map(|(path, blob, facts)| {
                let json = serde_json::to_string(facts).map_err(|_| unwritable())?;
                Ok((path, blob, json))
            })
            .collect::<Result<Vec<_>, Fault>>()?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR REPLACE INTO map_facts (workspace_id, repo, path, blob, facts) ",
        );
        query.push_values(rows, |mut row, (path, blob, json)| {
            row.push_bind(workspace)
                .push_bind(repo)
                .push_bind(path)
                .push_bind(blob)
                .push_bind(json);
        });
        query
            .build()
            .execute(&mut **transaction)
            .await
            .map_err(failed)?;
    }
    Ok(())
}

pub(super) async fn insert_entries(
    transaction: &mut Transaction<'_>,
    (workspace, repo, layer): (&str, &str, &str),
    entries: &[Entry],
) -> Result<(), Fault> {
    for batch in entries.chunks(BATCH) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO map_search (workspace_id, repo, layer, path, kind, name, body) ",
        );
        query.push_values(batch, |mut row, entry| {
            row.push_bind(workspace)
                .push_bind(repo)
                .push_bind(layer)
                .push_bind(&entry.file)
                .push_bind(&entry.kind)
                .push_bind(&entry.name)
                .push_bind(&entry.body);
        });
        query
            .build()
            .execute(&mut **transaction)
            .await
            .map_err(failed)?;
    }
    Ok(())
}

fn unwritable() -> Fault {
    Fault::new(code!("db.unwritable"), "A value can't be stored")
}
