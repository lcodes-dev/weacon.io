use serde::Serialize;
use sqlx::SqlitePool;

use crate::db::{as_json, new_id};

#[derive(Serialize)]
pub struct Machine {
    pub id: String,
    #[serde(skip)]
    pub user_id: String,
    pub name: String,
    pub os: String,
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
