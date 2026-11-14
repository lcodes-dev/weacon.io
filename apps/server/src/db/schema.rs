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
    // Chats, the events their machines report, and the commands their owners sent them. A
    // workspace counts the bytes its chats' events take.
    "
ALTER TABLE workspaces ADD COLUMN chat_bytes INTEGER NOT NULL DEFAULT 0;

CREATE TABLE chats (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    chat_id TEXT NOT NULL,
    owner_member TEXT NOT NULL,
    title TEXT NOT NULL,
    repo TEXT,
    machine_name TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0,
    core_seq INTEGER NOT NULL DEFAULT 0,
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
    PRIMARY KEY (workspace_id, chat_id, seq),
    UNIQUE (workspace_id, chat_id, core_seq),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id)
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
