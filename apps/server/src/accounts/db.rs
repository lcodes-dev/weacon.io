use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use crate::db::new_id;
use crate::groups;

const CODE_TRIES: i64 = 5;
const CODE_WAIT: i64 = 60;

#[derive(Clone, Serialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    pub is_owner: bool,
    pub teams: Vec<Membership>,
}

#[derive(Clone, Serialize)]
pub struct Membership {
    pub id: String,
    pub name: String,
    pub role: String,
}

pub async fn any_exist(pool: &SqlitePool) -> sqlx::Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM users) AS "found: bool""#).fetch_one(pool).await
}

pub async fn exists(pool: &SqlitePool, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM users WHERE email = ?) AS "found: bool""#, email)
        .fetch_one(pool)
        .await
}

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
    Ok(Some(User { id: user.id, email: user.email, name: user.name, is_owner: user.is_owner, teams }))
}

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

pub async fn end_code_wait(pool: &SqlitePool, email: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE sign_in_codes SET expires_at = expires_at - ? WHERE email = ?", CODE_WAIT, email)
        .execute(pool)
        .await?;
    Ok(())
}

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
    sqlx::query!("DELETE FROM sign_in_codes WHERE email = ?", email).execute(&mut *tx).await?;

    let existing =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = ?", email).fetch_optional(&mut *tx).await?;
    let user_id = match (existing, tried.name) {
        (Some(id), _) => id,
        (None, Some(name)) => register(&mut tx, email, &name, now).await?,
        (None, None) => return Ok(false),
    };
    sqlx::query!("INSERT INTO sessions (token, user_id, renewed_at) VALUES (?, ?, ?)", token, user_id, now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

async fn register(tx: &mut SqliteConnection, email: &str, name: &str, now: i64) -> sqlx::Result<String> {
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

pub async fn renew_session(pool: &SqlitePool, token: &str, now: i64, before: i64) -> sqlx::Result<bool> {
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
    sqlx::query!("DELETE FROM sessions WHERE token = ?", token).execute(pool).await?;
    Ok(())
}

#[derive(Serialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct Invite {
    pub id: String,
    pub email: String,
    pub expires_at: i64,
}

#[derive(Serialize)]
pub struct Invitation {
    pub team_name: String,
    pub invited_by: String,
    pub email: String,
    pub has_account: bool,
}

pub async fn rename_team(pool: &SqlitePool, team_id: &str, name: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE teams SET name = ? WHERE id = ?", name, team_id).execute(pool).await?;
    Ok(())
}

pub async fn members(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Vec<Member>> {
    sqlx::query_as!(
        Member,
        "SELECT u.id, u.name, u.email, m.role
         FROM team_members m JOIN users u ON u.id = m.user_id
         WHERE m.team_id = ?
         ORDER BY m.role = 'member', u.name",
        team_id
    )
    .fetch_all(pool)
    .await
}

pub async fn member_ids(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!("SELECT user_id FROM team_members WHERE team_id = ?", team_id).fetch_all(pool).await
}

pub async fn has_member(pool: &SqlitePool, team_id: &str, user_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM team_members WHERE team_id = ? AND user_id = ?) AS "found: bool""#,
        team_id,
        user_id
    )
    .fetch_one(pool)
    .await
}

pub async fn has_member_with_email(pool: &SqlitePool, team_id: &str, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM team_members m JOIN users u ON u.id = m.user_id
                          WHERE m.team_id = ? AND u.email = ?) AS "found: bool""#,
        team_id,
        email
    )
    .fetch_one(pool)
    .await
}

pub async fn remove_member(pool: &SqlitePool, team_id: &str, user_id: &str) -> sqlx::Result<bool> {
    let removed = sqlx::query!(
        "DELETE FROM team_members WHERE team_id = ? AND user_id = ? AND role = 'member'",
        team_id,
        user_id
    )
    .execute(pool)
    .await?;
    let removed = removed.rows_affected() > 0;
    if removed {
        groups::db::leave(pool, team_id, None, user_id).await?;
    }
    Ok(removed)
}

pub async fn invites(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Vec<Invite>> {
    sqlx::query_as!(
        Invite,
        "SELECT id, email, expires_at FROM invites WHERE team_id = ? ORDER BY email",
        team_id
    )
    .fetch_all(pool)
    .await
}

pub async fn save_invite(
    pool: &SqlitePool,
    team_id: &str,
    email: &str,
    token: &str,
    invited_by: &str,
    expires_at: i64,
) -> sqlx::Result<()> {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO invites (id, team_id, email, token, invited_by, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (team_id, email) DO UPDATE
         SET token = excluded.token, invited_by = excluded.invited_by,
             expires_at = excluded.expires_at",
        id,
        team_id,
        email,
        token,
        invited_by,
        expires_at
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_invite(pool: &SqlitePool, team_id: &str, invite_id: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM invites WHERE team_id = ? AND id = ?", team_id, invite_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn invitation(pool: &SqlitePool, token: &str, now: i64) -> sqlx::Result<Option<Invitation>> {
    sqlx::query_as!(
        Invitation,
        r#"SELECT t.name AS team_name, u.name AS invited_by, i.email,
                  EXISTS (SELECT 1 FROM users WHERE email = i.email) AS "has_account: bool"
           FROM invites i
           JOIN teams t ON t.id = i.team_id
           JOIN users u ON u.id = i.invited_by
           WHERE i.token = ? AND i.expires_at > ?"#,
        token,
        now
    )
    .fetch_optional(pool)
    .await
}

pub async fn accept_invite(pool: &SqlitePool, token: &str, user_id: &str) -> sqlx::Result<Option<String>> {
    let mut tx = pool.begin().await?;
    let team_id = sqlx::query_scalar!("DELETE FROM invites WHERE token = ? RETURNING team_id", token)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(team_id) = &team_id {
        sqlx::query!(
            "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'member')",
            team_id,
            user_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(team_id)
}
