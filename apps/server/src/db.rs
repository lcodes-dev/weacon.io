use serde::{Deserialize, Serialize};
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
    #[sqlx(skip)]
    pub teams: Vec<Team>,
}

#[derive(Serialize, FromRow)]
pub struct Team {
    pub id: i64,
    pub name: String,
    /// `owner` or `member`.
    pub role: String,
}

#[derive(Serialize, FromRow)]
pub struct Member {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub role: String,
}

#[derive(Serialize, FromRow)]
pub struct Invite {
    pub id: i64,
    pub email: String,
    pub expires_at: i64,
}

#[derive(Serialize, FromRow)]
pub struct Project {
    pub id: i64,
    pub name: String,
    /// `owner/name` on GitHub.
    pub repo: Option<String>,
}

/// A team's own GitHub App.
#[derive(Deserialize, FromRow)]
pub struct GithubApp {
    pub id: i64,
    pub slug: String,
    /// The private key that signs the app's tokens.
    pub pem: String,
}

/// What the person opening an invite link sees.
#[derive(Serialize, FromRow)]
pub struct Invitation {
    pub team_name: String,
    pub invited_by: String,
    pub email: String,
    /// Tells the web app to offer signing in, not registering.
    pub has_account: bool,
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
    let user: Option<User> = sqlx::query_as(
        "SELECT u.id, u.email, u.name, u.is_owner
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = ?",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    let Some(mut user) = user else {
        return Ok(None);
    };
    user.teams = sqlx::query_as(
        "SELECT t.id, t.name, m.role
         FROM team_members m JOIN teams t ON t.id = m.team_id
         WHERE m.user_id = ?
         ORDER BY t.id",
    )
    .bind(user.id)
    .fetch_all(pool)
    .await?;
    Ok(Some(user))
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

pub async fn rename_team(pool: &SqlitePool, team_id: i64, name: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE teams SET name = ? WHERE id = ?")
        .bind(name)
        .bind(team_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn members(pool: &SqlitePool, team_id: i64) -> sqlx::Result<Vec<Member>> {
    sqlx::query_as(
        "SELECT u.id, u.name, u.email, m.role
         FROM team_members m JOIN users u ON u.id = m.user_id
         WHERE m.team_id = ?
         ORDER BY m.role = 'member', u.name",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await
}

pub async fn is_member(pool: &SqlitePool, team_id: i64, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM team_members m JOIN users u ON u.id = m.user_id
                        WHERE m.team_id = ? AND u.email = ?)",
    )
    .bind(team_id)
    .bind(email)
    .fetch_one(pool)
    .await
}

/// Returns whether a member was removed. The team's owner never is.
pub async fn remove_member(pool: &SqlitePool, team_id: i64, user_id: i64) -> sqlx::Result<bool> {
    let removed = sqlx::query(
        "DELETE FROM team_members WHERE team_id = ? AND user_id = ? AND role = 'member'",
    )
    .bind(team_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(removed.rows_affected() > 0)
}

pub async fn invites(pool: &SqlitePool, team_id: i64) -> sqlx::Result<Vec<Invite>> {
    sqlx::query_as("SELECT id, email, expires_at FROM invites WHERE team_id = ? ORDER BY email")
        .bind(team_id)
        .fetch_all(pool)
        .await
}

/// Inviting an address again replaces its token and expiry.
pub async fn save_invite(
    pool: &SqlitePool,
    team_id: i64,
    email: &str,
    token: &str,
    invited_by: i64,
    expires_at: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO invites (team_id, email, token, invited_by, expires_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (team_id, email) DO UPDATE
         SET token = excluded.token, invited_by = excluded.invited_by,
             expires_at = excluded.expires_at",
    )
    .bind(team_id)
    .bind(email)
    .bind(token)
    .bind(invited_by)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_invite(pool: &SqlitePool, team_id: i64, invite_id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM invites WHERE team_id = ? AND id = ?")
        .bind(team_id)
        .bind(invite_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn invitation(
    pool: &SqlitePool,
    token: &str,
    now: i64,
) -> sqlx::Result<Option<Invitation>> {
    sqlx::query_as(
        "SELECT t.name AS team_name, u.name AS invited_by, i.email,
                EXISTS (SELECT 1 FROM users WHERE email = i.email) AS has_account
         FROM invites i
         JOIN teams t ON t.id = i.team_id
         JOIN users u ON u.id = i.invited_by
         WHERE i.token = ? AND i.expires_at > ?",
    )
    .bind(token)
    .bind(now)
    .fetch_optional(pool)
    .await
}

/// Uses the invite up. Returns the team joined, or `None` when the token is gone.
pub async fn accept_invite(
    pool: &SqlitePool,
    token: &str,
    user_id: i64,
) -> sqlx::Result<Option<i64>> {
    let mut tx = pool.begin().await?;
    let team_id: Option<i64> =
        sqlx::query_scalar("DELETE FROM invites WHERE token = ? RETURNING team_id")
            .bind(token)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(team_id) = team_id {
        sqlx::query(
            "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'member')",
        )
        .bind(team_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(team_id)
}

pub async fn projects(pool: &SqlitePool, team_id: i64) -> sqlx::Result<Vec<Project>> {
    sqlx::query_as("SELECT id, name, repo FROM projects WHERE team_id = ? ORDER BY name")
        .bind(team_id)
        .fetch_all(pool)
        .await
}

pub async fn create_project(
    pool: &SqlitePool,
    team_id: i64,
    name: &str,
    repo: Option<&str>,
    now: i64,
) -> sqlx::Result<Project> {
    sqlx::query_as(
        "INSERT INTO projects (team_id, name, repo, created_at) VALUES (?, ?, ?, ?)
         RETURNING id, name, repo",
    )
    .bind(team_id)
    .bind(name)
    .bind(repo)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Returns whether the team has that project.
pub async fn update_project(
    pool: &SqlitePool,
    team_id: i64,
    project_id: i64,
    name: &str,
    repo: Option<&str>,
) -> sqlx::Result<bool> {
    let updated =
        sqlx::query("UPDATE projects SET name = ?, repo = ? WHERE team_id = ? AND id = ?")
            .bind(name)
            .bind(repo)
            .bind(team_id)
            .bind(project_id)
            .execute(pool)
            .await?;
    Ok(updated.rows_affected() > 0)
}

pub async fn delete_project(pool: &SqlitePool, team_id: i64, project_id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM projects WHERE team_id = ? AND id = ?")
        .bind(team_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn github_app(pool: &SqlitePool, team_id: i64) -> sqlx::Result<Option<GithubApp>> {
    sqlx::query_as(
        "SELECT github_app_id AS id, github_app_slug AS slug, github_private_key AS pem
         FROM teams WHERE id = ? AND github_app_id IS NOT NULL",
    )
    .bind(team_id)
    .fetch_optional(pool)
    .await
}

/// `None` forgets the team's app.
pub async fn save_github_app(
    pool: &SqlitePool,
    team_id: i64,
    app: Option<&GithubApp>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE teams SET github_app_id = ?, github_app_slug = ?, github_private_key = ?
         WHERE id = ?",
    )
    .bind(app.map(|app| app.id))
    .bind(app.map(|app| &app.slug))
    .bind(app.map(|app| &app.pem))
    .bind(team_id)
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
