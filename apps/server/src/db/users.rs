use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use super::new_id;

const CODE_TRIES: i64 = 5;
/// Seconds before another sign-in code can be asked for.
const CODE_WAIT: i64 = 60;

#[derive(Serialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    pub is_owner: bool,
    pub teams: Vec<Membership>,
}

/// A team as one of its members sees it.
#[derive(Serialize)]
pub struct Membership {
    pub id: String,
    pub name: String,
    /// `owner` or `member`.
    pub role: String,
}

pub async fn any_exist(pool: &SqlitePool) -> sqlx::Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM users) AS "found: bool""#)
        .fetch_one(pool)
        .await
}

pub async fn exists(pool: &SqlitePool, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users WHERE email = ?) AS "found: bool""#,
        email
    )
    .fetch_one(pool)
    .await
}

/// The user of a session renewed after `since`.
pub async fn by_session(pool: &SqlitePool, token: &str, since: i64) -> sqlx::Result<Option<User>> {
    let user = sqlx::query!(
        r#"SELECT u.id, u.email, u.name, u.is_owner AS "is_owner: bool"
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.token = ? AND s.renewed_at > ?"#,
        token,
        since
    )
    .fetch_optional(pool)
    .await?;
    let Some(user) = user else {
        return Ok(None);
    };
    let teams = sqlx::query_as!(
        Membership,
        "SELECT t.id, t.name, m.role
         FROM team_members m JOIN teams t ON t.id = m.team_id
         WHERE m.user_id = ?
         ORDER BY t.id",
        user.id
    )
    .fetch_all(pool)
    .await?;
    Ok(Some(User {
        id: user.id,
        email: user.email,
        name: user.name,
        is_owner: user.is_owner,
        teams,
    }))
}

/// False when the address asked for a code less than a minute ago, whose code then stands.
/// Wrong tries stay counted while the earlier code is live, so asking again buys no guesses.
pub async fn save_code(
    pool: &SqlitePool,
    email: &str,
    code: &str,
    name: Option<&str>,
    now: i64,
    lifetime: i64,
) -> sqlx::Result<bool> {
    let saved = sqlx::query!(
        "INSERT INTO sign_in_codes (email, code, name, expires_at) VALUES (?1, ?2, ?3, ?4 + ?5)
         ON CONFLICT (email) DO UPDATE
         SET code = excluded.code, name = excluded.name, expires_at = excluded.expires_at,
             attempts = CASE WHEN sign_in_codes.expires_at > ?4 THEN sign_in_codes.attempts ELSE 0 END
         WHERE sign_in_codes.expires_at - ?5 + ?6 <= ?4",
        email,
        code,
        name,
        now,
        lifetime,
        CODE_WAIT
    )
    .execute(pool)
    .await?;
    Ok(saved.rows_affected() > 0)
}

/// Lets a code be asked for again at once, as when its mail couldn't be sent.
pub async fn end_code_wait(pool: &SqlitePool, email: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE sign_in_codes SET expires_at = expires_at - ? WHERE email = ?",
        CODE_WAIT,
        email
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether the code was right, which starts the session `token`. A registration's code also
/// creates the account and its team.
pub async fn sign_in(
    pool: &SqlitePool,
    email: &str,
    code: &str,
    token: &str,
    now: i64,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let tried = sqlx::query!(
        "UPDATE sign_in_codes SET attempts = attempts + 1 WHERE email = ?
         RETURNING code, name, expires_at, attempts",
        email
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(tried) = tried else {
        return Ok(false);
    };
    if tried.code != code || tried.expires_at <= now || tried.attempts > CODE_TRIES {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query!("DELETE FROM sign_in_codes WHERE email = ?", email)
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query_scalar!("SELECT id FROM users WHERE email = ?", email)
        .fetch_optional(&mut *tx)
        .await?;
    let user_id = match (existing, tried.name) {
        (Some(id), _) => id,
        (None, Some(name)) => register(&mut tx, email, &name, now).await?,
        (None, None) => return Ok(false),
    };
    sqlx::query!(
        "INSERT INTO sessions (token, user_id, renewed_at) VALUES (?, ?, ?)",
        token,
        user_id,
        now
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// The first account on a server is its owner, and every account starts with a team of its own.
async fn register(
    tx: &mut SqliteConnection,
    email: &str,
    name: &str,
    now: i64,
) -> sqlx::Result<String> {
    let (user_id, team_id) = (new_id(), new_id());
    sqlx::query!(
        "INSERT INTO users (id, email, name, is_owner, created_at)
         VALUES (?, ?, ?, NOT EXISTS (SELECT 1 FROM users), ?)",
        user_id,
        email,
        name,
        now
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO teams (id, name, created_at) VALUES (?, ?, ?)",
        team_id,
        format!("{name}'s team"),
        now
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
        team_id,
        user_id
    )
    .execute(&mut *tx)
    .await?;
    Ok(user_id)
}

/// Starts the session's year again, unless that was done after `before`. Whether it was.
pub async fn renew_session(
    pool: &SqlitePool,
    token: &str,
    now: i64,
    before: i64,
) -> sqlx::Result<bool> {
    let renewed = sqlx::query!(
        "UPDATE sessions SET renewed_at = ? WHERE token = ? AND renewed_at < ?",
        now,
        token,
        before
    )
    .execute(pool)
    .await?;
    Ok(renewed.rows_affected() > 0)
}

pub async fn sign_out(pool: &SqlitePool, token: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM sessions WHERE token = ?", token)
        .execute(pool)
        .await?;
    Ok(())
}
