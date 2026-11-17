use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{FromRow, SqlitePool};

use crate::mail::Smtp;

const CODE_TRIES: i64 = 5;

pub async fn open(path: &str) -> sqlx::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    migrated(SqlitePool::connect_with(options).await?).await
}

/// For tests. One connection, because each in-memory connection is its own database.
pub async fn memory() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    migrated(pool).await.unwrap()
}

async fn migrated(pool: SqlitePool) -> sqlx::Result<SqlitePool> {
    sqlx::migrate!()
        .run(&pool)
        .await
        .map_err(|error| sqlx::Error::Migrate(Box::new(error)))?;
    Ok(pool)
}

#[derive(Serialize, FromRow)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub is_owner: bool,
    pub team_id: i64,
    pub team_name: String,
}

pub async fn has_users(pool: &SqlitePool) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(pool)
        .await
}

pub async fn user_exists(pool: &SqlitePool, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email = ?)")
        .bind(email)
        .fetch_one(pool)
        .await
}

pub async fn user_by_session(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(
        "SELECT u.id, u.email, u.name, u.is_owner, t.id AS team_id, t.name AS team_name
         FROM sessions s
         JOIN users u ON u.id = s.user_id
         JOIN team_members m ON m.user_id = u.id
         JOIN teams t ON t.id = m.team_id
         WHERE s.token = ?
         ORDER BY t.id
         LIMIT 1",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
}

pub async fn save_code(
    pool: &SqlitePool,
    email: &str,
    code: &str,
    name: Option<&str>,
    expires_at: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO sign_in_codes (email, code, name, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(email)
    .bind(code)
    .bind(name)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns whether the code was right. A registration's code also creates the account and team.
pub async fn sign_in(
    pool: &SqlitePool,
    email: &str,
    code: &str,
    token: &str,
    now: i64,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, Option<String>, i64, i64)> = sqlx::query_as(
        "UPDATE sign_in_codes SET attempts = attempts + 1 WHERE email = ?
         RETURNING code, name, expires_at, attempts",
    )
    .bind(email)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((expected, name, expires_at, attempts)) = row else {
        return Ok(false);
    };
    if expected != code || expires_at <= now || attempts > CODE_TRIES {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query("DELETE FROM sign_in_codes WHERE email = ?")
        .bind(email)
        .execute(&mut *tx)
        .await?;

    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
        .bind(email)
        .fetch_optional(&mut *tx)
        .await?;
    let user_id = match (existing, name) {
        (Some(id), _) => id,
        (None, Some(name)) => create_user(&mut tx, email, &name, now).await?,
        (None, None) => return Ok(false),
    };
    sqlx::query("INSERT INTO sessions (token, user_id, created_at) VALUES (?, ?, ?)")
        .bind(token)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

async fn create_user(
    tx: &mut sqlx::SqliteConnection,
    email: &str,
    name: &str,
    now: i64,
) -> sqlx::Result<i64> {
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, name, is_owner, created_at)
         VALUES (?, ?, NOT EXISTS (SELECT 1 FROM users), ?) RETURNING id",
    )
    .bind(email)
    .bind(name)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    let team_id: i64 =
        sqlx::query_scalar("INSERT INTO teams (name, created_at) VALUES (?, ?) RETURNING id")
            .bind(format!("{name}'s team"))
            .bind(now)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(team_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(user_id)
}

pub async fn sign_out(pool: &SqlitePool, token: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn smtp(pool: &SqlitePool) -> sqlx::Result<Option<Smtp>> {
    sqlx::query_as(
        "SELECT smtp_host AS host, smtp_port AS port, smtp_security AS security,
                smtp_username AS username, smtp_password AS password, mail_from AS \"from\"
         FROM config WHERE smtp_host IS NOT NULL",
    )
    .fetch_optional(pool)
    .await
}

/// `None` clears the SMTP settings.
pub async fn save_smtp(pool: &SqlitePool, smtp: Option<&Smtp>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE config SET smtp_host = ?, smtp_port = ?, smtp_security = ?,
                           smtp_username = ?, smtp_password = ?, mail_from = ?",
    )
    .bind(smtp.map(|smtp| &smtp.host))
    .bind(smtp.map(|smtp| smtp.port))
    .bind(smtp.map(|smtp| smtp.security))
    .bind(smtp.and_then(|smtp| smtp.username.as_ref()))
    .bind(smtp.and_then(|smtp| smtp.password.as_ref()))
    .bind(smtp.map(|smtp| &smtp.from))
    .execute(pool)
    .await?;
    Ok(())
}
