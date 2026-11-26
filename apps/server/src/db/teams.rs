use serde::Serialize;
use sqlx::SqlitePool;

use super::{groups, new_id};

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

pub async fn rename(pool: &SqlitePool, team_id: &str, name: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE teams SET name = ? WHERE id = ?", name, team_id)
        .execute(pool)
        .await?;
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
    sqlx::query_scalar!(
        "SELECT user_id FROM team_members WHERE team_id = ?",
        team_id
    )
    .fetch_all(pool)
    .await
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

pub async fn has_member_with_email(
    pool: &SqlitePool,
    team_id: &str,
    email: &str,
) -> sqlx::Result<bool> {
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
        groups::leave(pool, team_id, None, user_id).await?;
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
    sqlx::query!(
        "DELETE FROM invites WHERE team_id = ? AND id = ?",
        team_id,
        invite_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn invitation(
    pool: &SqlitePool,
    token: &str,
    now: i64,
) -> sqlx::Result<Option<Invitation>> {
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

pub async fn accept_invite(
    pool: &SqlitePool,
    token: &str,
    user_id: &str,
) -> sqlx::Result<Option<String>> {
    let mut tx = pool.begin().await?;
    let team_id = sqlx::query_scalar!(
        "DELETE FROM invites WHERE token = ? RETURNING team_id",
        token
    )
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
