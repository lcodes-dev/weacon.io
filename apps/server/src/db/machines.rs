use serde::Serialize;
use sqlx::SqlitePool;

use super::{as_json, new_id};

/// A person's own computer or virtual machine, as its page shows it.
#[derive(Serialize)]
pub struct Machine {
    pub id: String,
    #[serde(skip)]
    pub user_id: String,
    pub name: String,
    pub os: String,
    /// What its core last said of its agents, as a JSON list.
    #[serde(serialize_with = "as_json")]
    pub agents: String,
    pub created_at: i64,
    pub seen_at: Option<i64>,
}

pub async fn list(pool: &SqlitePool, user_id: &str) -> sqlx::Result<Vec<Machine>> {
    sqlx::query_as!(
        Machine,
        "SELECT id, user_id, name, os, agents, created_at, seen_at
         FROM machines WHERE user_id = ? ORDER BY id",
        user_id
    )
    .fetch_all(pool)
    .await
}

pub async fn by_token(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<Machine>> {
    sqlx::query_as!(
        Machine,
        "SELECT id, user_id, name, os, agents, created_at, seen_at FROM machines WHERE token = ?",
        token
    )
    .fetch_optional(pool)
    .await
}

/// `machine_id` is the machine whose core asked for the code, when one did.
pub async fn add_code(
    pool: &SqlitePool,
    code: &str,
    user_id: &str,
    machine_id: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO machine_codes (code, user_id, machine_id, created_at)
         VALUES (?, ?, ?, unixepoch())",
        code,
        user_id,
        machine_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Uses up a pairing code made after `since` for a new machine of whoever asked for the code,
/// and gives who that is. None when the code is unknown, used or older.
pub async fn pair(
    pool: &SqlitePool,
    code: &str,
    since: i64,
    name: &str,
    os: &str,
    token: &str,
) -> sqlx::Result<Option<String>> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let asked = sqlx::query!(
        "DELETE FROM machine_codes WHERE code = ? RETURNING user_id, created_at",
        code
    )
    .fetch_optional(&mut *tx)
    .await?;
    let asked = asked.filter(|asked| asked.created_at > since);
    if let Some(asked) = &asked {
        let id = new_id();
        sqlx::query!(
            "INSERT INTO machines (id, user_id, name, os, token, created_at)
             VALUES (?, ?, ?, ?, ?, unixepoch())",
            id,
            asked.user_id,
            name,
            os,
            token
        )
        .execute(&mut *tx)
        .await?;
    }
    // An old code is used up all the same; a machine that couldn't be saved leaves its code.
    tx.commit().await?;
    Ok(asked.map(|asked| asked.user_id))
}

pub async fn delete(pool: &SqlitePool, user_id: &str, id: &str) -> sqlx::Result<bool> {
    let deleted = sqlx::query!(
        "DELETE FROM machines WHERE id = ? AND user_id = ?",
        id,
        user_id
    )
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected() > 0)
}

/// Notes that a machine's core is or was just connected, and what it says of its agents, when it did.
pub async fn see(pool: &SqlitePool, id: &str, agents: Option<&str>) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE machines SET seen_at = unixepoch(), agents = COALESCE(?, agents) WHERE id = ?",
        agents,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}
