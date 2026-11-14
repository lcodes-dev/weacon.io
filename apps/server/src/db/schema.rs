use sqlx::{AssertSqlSafe, SqlitePool};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::failed;

/// Each step brings the schema from its index to the next version; steps are only appended.
pub(super) const STEPS: &[&str] = &[
    "
CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    name TEXT NOT NULL
) STRICT;

CREATE TABLE members (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    member_id TEXT NOT NULL,
    email TEXT NOT NULL,
    PRIMARY KEY (workspace_id, member_id),
    UNIQUE (workspace_id, email)
) STRICT;
",
    "
CREATE TABLE sign_in_codes (
    workspace_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    sent_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL,
    window_start INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, member_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE sessions (
    workspace_id TEXT NOT NULL,
    token_hash BLOB NOT NULL,
    member_id TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, token_hash),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE changes (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;
",
    "
DROP TABLE sign_in_codes;

CREATE TABLE sign_in_challenges (
    workspace_id TEXT NOT NULL,
    challenge_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, challenge_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;
",
    // Account ids in the format of `app::new_id`.
    "
CREATE TABLE accounts (
    account_id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE
) STRICT;

INSERT INTO accounts (account_id, email)
SELECT lower(hex(randomblob(16))), email FROM (SELECT DISTINCT email FROM members);

ALTER TABLE members ADD COLUMN account_id TEXT REFERENCES accounts (account_id);

UPDATE members SET account_id =
    (SELECT account_id FROM accounts WHERE accounts.email = members.email);
",
    // A person signs in to their account, not to one workspace.
    "
CREATE UNIQUE INDEX members_account ON members (workspace_id, account_id);

DROP TABLE sign_in_challenges;
DROP TABLE sessions;

CREATE TABLE sign_in_challenges (
    challenge_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    token_hash BLOB PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    expires_at INTEGER NOT NULL
) STRICT;
",
    // Tasks, each with the versions it went through.
    "
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
    PRIMARY KEY (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, accepted_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE changes_with_tasks (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version)
) STRICT;

INSERT INTO changes_with_tasks (workspace_id, seq, kind, member_id)
SELECT workspace_id, seq, kind, member_id FROM changes;

DROP TABLE changes;
ALTER TABLE changes_with_tasks RENAME TO changes;
",
    // What each member may do, and whether an account has had its Personal workspace made.
    "
ALTER TABLE members ADD COLUMN role TEXT NOT NULL DEFAULT 'member';
ALTER TABLE accounts ADD COLUMN personal_made INTEGER NOT NULL DEFAULT 0;
",
    // A challenge is for an address, whose account a right code may make. A server's owner is
    // its first account; one from before is that of the first member added.
    "
DROP TABLE sign_in_challenges;

CREATE TABLE sign_in_challenges (
    challenge_id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL
) STRICT;

ALTER TABLE accounts ADD COLUMN owner INTEGER NOT NULL DEFAULT 0;
UPDATE accounts SET owner = 1 WHERE account_id = coalesce(
    (SELECT account_id FROM members ORDER BY rowid LIMIT 1),
    (SELECT account_id FROM accounts ORDER BY rowid LIMIT 1)
);
CREATE UNIQUE INDEX accounts_owner ON accounts (owner) WHERE owner = 1;
",
    // Only so many live codes are mailed to an address; its other challenges take those codes.
    "
ALTER TABLE sign_in_challenges ADD COLUMN mailed INTEGER NOT NULL DEFAULT 1;
CREATE INDEX sign_in_challenges_email ON sign_in_challenges (email);
",
    // Docs, each revision whole, with the pins it links to.
    "
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

CREATE TABLE changes_with_docs (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    doc_id TEXT,
    doc_version INTEGER,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, doc_id, doc_version)
        REFERENCES doc_versions (workspace_id, doc_id, version)
) STRICT;

INSERT INTO changes_with_docs (workspace_id, seq, kind, member_id, task_id, task_version)
SELECT workspace_id, seq, kind, member_id, task_id, task_version FROM changes;

DROP TABLE changes;
ALTER TABLE changes_with_docs RENAME TO changes;
",
    // Machines paired with an account, and the pairings under way. A confirmed pairing names
    // its account and machine, and stays until it expires so that its core may claim again.
    "
CREATE TABLE machines (
    machine_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    machine_key TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    paired_at INTEGER NOT NULL
) STRICT;

CREATE TABLE pairings (
    pairing_id TEXT PRIMARY KEY,
    code_hash BLOB NOT NULL UNIQUE,
    secret_hash BLOB NOT NULL,
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    machine_key TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    account_id TEXT REFERENCES accounts (account_id),
    machine_id TEXT,
    confirmed_at INTEGER,
    claimed INTEGER NOT NULL DEFAULT 0
) STRICT;
",
    // Chats, the events their machines report, the prompts that wait for their owners and the
    // commands their owners sent them. A workspace counts the bytes its chats' events take.
    "
ALTER TABLE workspaces ADD COLUMN chat_bytes INTEGER NOT NULL DEFAULT 0;

CREATE TABLE chats (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    chat_id TEXT NOT NULL,
    owner_member TEXT NOT NULL,
    title TEXT NOT NULL,
    repo TEXT,
    machine_id TEXT NOT NULL,
    machine_name TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0,
    core_seq INTEGER NOT NULL DEFAULT 0,
    run_id TEXT,
    run_status TEXT,
    PRIMARY KEY (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, owner_member) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chats_updated ON chats (workspace_id, updated_at);
CREATE INDEX chats_machine ON chats (workspace_id, machine_id);

CREATE TABLE chat_events (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    core_seq INTEGER,
    body TEXT NOT NULL,
    at INTEGER NOT NULL,
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

CREATE TABLE chat_requests (
    workspace_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    action_hash BLOB NOT NULL,
    sent_at INTEGER NOT NULL,
    open INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (workspace_id, request_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chat_requests_open ON chat_requests (workspace_id, chat_id) WHERE open = 1;
",
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
                "changes",
                "machines",
                "pairings",
                "chats",
                "chat_events",
                "chat_waiting",
                "chat_requests"
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
