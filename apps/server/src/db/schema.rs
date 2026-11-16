use sqlx::{AssertSqlSafe, SqlitePool};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::failed;

/// Every table and index. Until the first release a database is never carried from one schema
/// to another: a change is made here, and a database made before it is deleted.
const SCHEMA: &str = "
CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    name TEXT NOT NULL
) STRICT;

CREATE TABLE accounts (
    account_id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    personal_made INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE members (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    member_id TEXT NOT NULL,
    email TEXT NOT NULL,
    account_id TEXT REFERENCES accounts (account_id),
    role TEXT NOT NULL DEFAULT 'member',
    PRIMARY KEY (workspace_id, member_id),
    UNIQUE (workspace_id, email)
) STRICT;

CREATE UNIQUE INDEX members_account ON members (workspace_id, account_id);

CREATE TABLE invitations (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    email TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, email)
) STRICT;

CREATE TABLE sign_in_challenges (
    challenge_id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    code TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL
) STRICT;

CREATE INDEX sign_in_challenges_email ON sign_in_challenges (email);

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE machines (
    machine_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    paired_at INTEGER NOT NULL,
    core_build TEXT,
    core_offers TEXT
) STRICT;

CREATE TABLE pairings (
    pairing_id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    secret TEXT NOT NULL,
    token TEXT NOT NULL,
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    account_id TEXT REFERENCES accounts (account_id),
    machine_id TEXT,
    confirmed_at INTEGER,
    claimed INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Each row is what a machine's core says its owner shares, so the machine is plain text.
CREATE TABLE shared_machines (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    machine_id TEXT NOT NULL,
    PRIMARY KEY (workspace_id, machine_id)
) STRICT;

CREATE TABLE tasks (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    task_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    order_key TEXT NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, task_id),
    UNIQUE (workspace_id, order_key)
) STRICT;

CREATE TABLE task_versions (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    author_kind TEXT NOT NULL,
    author_member TEXT NOT NULL,
    accepted_by TEXT NOT NULL,
    at INTEGER NOT NULL,
    agent TEXT,
    PRIMARY KEY (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, accepted_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE docs (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    doc_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, doc_id)
) STRICT;

CREATE TABLE doc_versions (
    workspace_id TEXT NOT NULL,
    doc_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    commit_id TEXT,
    author_kind TEXT NOT NULL,
    author_member TEXT NOT NULL,
    accepted_by TEXT NOT NULL,
    at INTEGER NOT NULL,
    agent TEXT,
    repo_id TEXT,
    PRIMARY KEY (workspace_id, doc_id, version),
    FOREIGN KEY (workspace_id, doc_id) REFERENCES docs (workspace_id, doc_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, accepted_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE doc_pins (
    workspace_id TEXT NOT NULL,
    doc_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    position INTEGER NOT NULL,
    pin TEXT NOT NULL,
    PRIMARY KEY (workspace_id, doc_id, version, position),
    FOREIGN KEY (workspace_id, doc_id, version)
        REFERENCES doc_versions (workspace_id, doc_id, version)
) STRICT;

-- The proposing machine is plain text, as it outlives the machine's pairing.
CREATE TABLE proposals (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    proposal_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    doc_id TEXT,
    task_id TEXT,
    new_id TEXT,
    base INTEGER,
    change TEXT,
    change_hash BLOB NOT NULL,
    snapshot TEXT,
    author_member TEXT NOT NULL,
    agent TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    state TEXT NOT NULL,
    decided_by TEXT,
    version INTEGER,
    at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, proposal_id),
    FOREIGN KEY (workspace_id, doc_id) REFERENCES docs (workspace_id, doc_id),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, decided_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX proposals_open ON proposals (workspace_id, author_member) WHERE state = 'open';

CREATE TABLE chats (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    chat_id TEXT NOT NULL,
    owner_member TEXT NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0,
    run_id TEXT,
    run_status TEXT,
    called_by TEXT,
    called_at INTEGER,
    called_on TEXT,
    called_for TEXT,
    PRIMARY KEY (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, owner_member) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chats_updated ON chats (workspace_id, updated_at);

CREATE TABLE chat_events (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    core_seq INTEGER,
    body TEXT NOT NULL,
    at INTEGER NOT NULL,
    member_id TEXT,
    machine_id TEXT,
    PRIMARY KEY (workspace_id, chat_id, seq),
    UNIQUE (workspace_id, chat_id, core_seq),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id)
) STRICT;

CREATE TABLE chat_waiting (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    prompt INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, chat_id, run_id, prompt),
    FOREIGN KEY (workspace_id, chat_id, seq) REFERENCES chat_events (workspace_id, chat_id, seq)
) STRICT;

CREATE TABLE chat_tasks (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id)
) STRICT;

CREATE TABLE chat_legs (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    machine_name TEXT NOT NULL,
    agent TEXT NOT NULL,
    repo TEXT,
    core_seq INTEGER NOT NULL DEFAULT 0,
    detached INTEGER NOT NULL DEFAULT 0,
    quoted_seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, chat_id, machine_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chat_legs_machine ON chat_legs (workspace_id, machine_id);

CREATE TABLE chat_calls (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (workspace_id, chat_id, request_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE changes (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    doc_id TEXT,
    doc_version INTEGER,
    proposal_id TEXT,
    chat_id TEXT,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, doc_id, doc_version)
        REFERENCES doc_versions (workspace_id, doc_id, version),
    FOREIGN KEY (workspace_id, proposal_id) REFERENCES proposals (workspace_id, proposal_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id)
) STRICT;

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
pub(super) async fn prepare(writer: &SqlitePool) -> Result<(), Fault> {
    let mut transaction = writer.begin_with("BEGIN IMMEDIATE").await.map_err(failed)?;
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
            "The database was made by another build of the server; delete it to start again",
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
    use weacon_testing::TempDir;

    use crate::db::Db;

    #[tokio::test]
    async fn a_database_with_this_schema_opens_again_and_one_with_another_is_refused() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        Db::create(&path).await.unwrap().close().await;
        let db = Db::open(&path).await.unwrap();
        sqlx::raw_sql("PRAGMA user_version = 27")
            .execute(&db.writer)
            .await
            .unwrap();
        db.close().await;

        let error = Db::open(&path).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.other_schema");
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

    // The tables FTS5 makes for a virtual table's rows, named after it, each with the first
    // column FTS5 gives it; they can't lead with the workspace, so they are left out only beside
    // a virtual table whose first column does.
    const SHADOWS: [(&str, &str); 5] = [
        ("data", "id"),
        ("idx", "segid"),
        ("content", "id"),
        ("docsize", "id"),
        ("config", "k"),
    ];

    async fn tables(db: &Db) -> Vec<String> {
        let all: Vec<(String, bool)> = sqlx::query_as(
            "SELECT name, sql LIKE 'CREATE VIRTUAL TABLE %' FROM sqlite_schema \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&db.readers)
        .await
        .unwrap();
        let first = "SELECT name FROM pragma_table_info(?) WHERE cid = 0";
        let mut shadows = Vec::new();
        for (name, virtual_table) in &all {
            if !*virtual_table || first_columns(db, first, name).await != ["workspace_id"] {
                continue;
            }
            for (suffix, column) in SHADOWS {
                let shadow = format!("{name}_{suffix}");
                let made = first_columns(db, first, &shadow).await == [column];
                shadows.extend(made.then_some(shadow));
            }
        }
        all.into_iter()
            .map(|(name, _)| name)
            .filter(|name| !shadows.contains(name))
            .collect()
    }

    #[tokio::test]
    async fn only_a_virtual_table_led_by_the_workspace_hides_its_shadow_tables() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let before = tables(&db).await;
        sqlx::raw_sql(
            "CREATE VIRTUAL TABLE led USING fts5(workspace_id UNINDEXED, body); \
             CREATE VIRTUAL TABLE stray USING fts5(body, workspace_id UNINDEXED); \
             CREATE TABLE led_extra (body TEXT); \
             CREATE VIRTUAL TABLE bare USING fts5(workspace_id UNINDEXED, body, content=''); \
             CREATE TABLE bare_content (body TEXT)",
        )
        .execute(&db.writer)
        .await
        .unwrap();

        let added: Vec<String> = tables(&db).await.split_off(before.len());
        assert_eq!(
            added,
            [
                "led",
                "stray",
                "stray_data",
                "stray_idx",
                "stray_content",
                "stray_docsize",
                "stray_config",
                "led_extra",
                "bare",
                "bare_content"
            ]
        );
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
                "accounts",
                "members",
                "invitations",
                "sign_in_challenges",
                "sessions",
                "machines",
                "pairings",
                "shared_machines",
                "tasks",
                "task_versions",
                "docs",
                "doc_versions",
                "doc_pins",
                "proposals",
                "chats",
                "chat_events",
                "chat_waiting",
                "chat_tasks",
                "chat_legs",
                "chat_calls",
                "changes",
                "search",
                "search_rows"
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
