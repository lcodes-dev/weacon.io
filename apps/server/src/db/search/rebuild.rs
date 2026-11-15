//! Indexes a database again when its search rows are missing, as after a restore of a copy
//! made without them.

use sqlx::SqlitePool;
use weacon_observability::Fault;

use crate::db::failed;

/// Whether a task, a doc or a made chat has no search row, or the rows and the index differ.
const MISSING: &str = "
SELECT EXISTS (SELECT 1 FROM tasks AS t WHERE NOT EXISTS (SELECT 1 FROM search_rows AS r
           WHERE r.workspace_id = t.workspace_id AND r.kind = 'task' AND r.item = t.task_id))
    OR EXISTS (SELECT 1 FROM docs AS d WHERE NOT EXISTS (SELECT 1 FROM search_rows AS r
           WHERE r.workspace_id = d.workspace_id AND r.kind = 'doc' AND r.item = d.doc_id))
    OR EXISTS (SELECT 1 FROM chats AS c
           WHERE EXISTS (SELECT 1 FROM changes AS h
                   WHERE h.workspace_id = c.workspace_id AND h.chat_id = c.chat_id)
               AND NOT EXISTS (SELECT 1 FROM search_rows AS r
                   WHERE r.workspace_id = c.workspace_id AND r.kind = 'chat'
                       AND r.item = c.chat_id))
    OR (SELECT COUNT(*) FROM search) != (SELECT COUNT(*) FROM search_rows)
";

// What a write indexes, read from every table: each task's title, each doc's title and current
// body with its pins, each made chat's title, and the first 8192 characters of each message.
const REBUILD: &str = "
DELETE FROM search;
DELETE FROM search_rows;

CREATE TEMP TABLE found AS
SELECT ROW_NUMBER() OVER () AS row, * FROM (
    SELECT workspace_id, 'task' AS kind, task_id AS item, title, '' AS body, at
    FROM tasks JOIN task_versions USING (workspace_id, task_id, version)
    UNION ALL
    SELECT workspace_id, 'doc', doc_id, title, body || COALESCE(
        (SELECT char(10) || group_concat(pin, char(10) ORDER BY position) FROM doc_pins
         WHERE doc_pins.workspace_id = docs.workspace_id AND doc_pins.doc_id = docs.doc_id
             AND doc_pins.version = docs.version), ''), at
    FROM docs JOIN doc_versions USING (workspace_id, doc_id, version)
    UNION ALL
    SELECT workspace_id, 'chat', chat_id, title, '', created_at
    FROM chats AS c WHERE EXISTS (SELECT 1 FROM changes AS h
        WHERE h.workspace_id = c.workspace_id AND h.chat_id = c.chat_id)
    UNION ALL
    SELECT workspace_id, 'message', chat_id || '/' || seq, '', substr(text, 1, 8192), at
    FROM (SELECT workspace_id, chat_id, seq, at,
              CASE
                  WHEN json_extract(body, '$.kind') IN ('prompt', 'outside_prompt')
                      THEN json_extract(body, '$.text')
                  WHEN json_extract(body, '$.kind') = 'run'
                      AND json_extract(body, '$.update.event.event') = 'text'
                      THEN json_extract(body, '$.update.event.text')
              END AS text
          FROM chat_events WHERE json_valid(body))
    WHERE text IS NOT NULL
);

INSERT INTO search (rowid, workspace_id, kind, item, title, body)
SELECT row, workspace_id, kind, item, replace(replace(title, char(1), ' '), char(2), ' '),
    replace(replace(body, char(1), ' '), char(2), ' ')
FROM found;

INSERT INTO search_rows (workspace_id, kind, item, row, at)
SELECT workspace_id, kind, item, row, at FROM found;

DROP TABLE found;
";

/// Indexes everything again when search rows are missing; gives whether it did.
pub(crate) async fn catch_up(writer: &SqlitePool) -> Result<bool, Fault> {
    let mut transaction = writer.begin_with("BEGIN IMMEDIATE").await.map_err(failed)?;
    let missing: bool = sqlx::query_scalar(MISSING)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
    if missing {
        sqlx::raw_sql(REBUILD)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
    }
    transaction.commit().await.map_err(failed)?;
    Ok(missing)
}
