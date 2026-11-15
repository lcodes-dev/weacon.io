// The text search finds: each task's title, and each doc's title and current body with its
// pins, without the bytes `db::search` marks matches with. Each row's id, and when its
// source last changed, are in `search_rows`.
pub(crate) const SEARCH: &str = "
CREATE VIRTUAL TABLE search USING fts5(
    workspace_id UNINDEXED,
    kind UNINDEXED,
    item UNINDEXED,
    title,
    body,
    tokenize = 'trigram remove_diacritics 1'
);

CREATE TABLE search_rows (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    kind TEXT NOT NULL,
    item TEXT NOT NULL,
    row INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, kind, item)
) STRICT;

CREATE INDEX search_rows_by_row ON search_rows (workspace_id, row);

INSERT INTO search (workspace_id, kind, item, title, body)
SELECT workspace_id, 'task', task_id, replace(replace(title, char(1), ' '), char(2), ' '), ''
FROM tasks;

INSERT INTO search (workspace_id, kind, item, title, body)
SELECT workspace_id, 'doc', doc_id,
    replace(replace(title, char(1), ' '), char(2), ' '),
    replace(replace(body || COALESCE(
        (SELECT char(10) || group_concat(pin, char(10) ORDER BY position) FROM doc_pins
         WHERE doc_pins.workspace_id = docs.workspace_id AND doc_pins.doc_id = docs.doc_id
             AND doc_pins.version = docs.version), ''), char(1), ' '), char(2), ' ')
FROM docs JOIN doc_versions USING (workspace_id, doc_id, version);

INSERT INTO search_rows (workspace_id, kind, item, row, at)
SELECT workspace_id, kind, item, rowid, COALESCE(
    (SELECT at FROM tasks JOIN task_versions USING (workspace_id, task_id, version)
     WHERE kind = 'task' AND tasks.workspace_id = search.workspace_id
         AND tasks.task_id = search.item),
    (SELECT at FROM docs JOIN doc_versions USING (workspace_id, doc_id, version)
     WHERE kind = 'doc' AND docs.workspace_id = search.workspace_id
         AND docs.doc_id = search.item),
    0)
FROM search;
";

// Chats join the search: each chat by its title, and each message its log keeps, a person's
// prompt or an agent's text, as `chat/seq`. An event that isn't JSON is left out.
pub(crate) const SEARCH_CHATS: &str = "
CREATE TEMP TABLE found AS
SELECT (SELECT COALESCE(MAX(rowid), 0) FROM search) + ROW_NUMBER() OVER () AS row, *
FROM (
    SELECT workspace_id, 'chat' AS kind, chat_id AS item, title, '' AS body, created_at AS at
    FROM chats
    UNION ALL
    SELECT workspace_id, 'message', chat_id || '/' || seq, '', text, at
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
