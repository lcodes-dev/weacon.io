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
