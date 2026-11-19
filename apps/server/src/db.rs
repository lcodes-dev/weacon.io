use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};

use crate::mail::Smtp;
use crate::map::{Reading, Symbol};
use crate::writer::Writer;

const CODE_TRIES: i64 = 5;
/// Seconds before another sign-in code can be asked for.
const CODE_WAIT: i64 = 60;

/// A UUIDv7: unique, and sorted by when it was made.
fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub async fn open(path: &str) -> sqlx::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    #[cfg(unix)]
    if let Err(error) = keep_private(path) {
        tracing::warn!("{path} could not be made private: {error}");
    }
    migrated(SqlitePool::connect_with(options).await?).await
}

/// Only the server's account may read the database: it holds keys, sessions and source code.
#[cfg(unix)]
fn keep_private(path: &str) -> std::io::Result<()> {
    use std::fs::{OpenOptions, Permissions, set_permissions};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut create = OpenOptions::new();
    create.create(true).append(true).mode(0o600).open(path)?;
    // SQLite gives its two side files the database's own permissions.
    for file in [
        path.to_owned(),
        format!("{path}-wal"),
        format!("{path}-shm"),
    ] {
        if std::fs::exists(&file)? {
            set_permissions(&file, Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
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

#[derive(Serialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    pub is_owner: bool,
    pub teams: Vec<Team>,
}

#[derive(Serialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    /// `owner` or `member`.
    pub role: String,
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
pub struct Project {
    pub id: String,
    pub name: String,
    /// `owner/name` on GitHub.
    pub repo: Option<String>,
    pub branches: Vec<Branch>,
}

/// A branch a project tracks. Its map is behind while the two commits differ.
#[derive(Serialize)]
pub struct Branch {
    #[serde(skip)]
    pub project_id: String,
    pub name: String,
    /// The latest commit GitHub has.
    pub head_sha: Option<String>,
    /// The commit the map describes.
    pub commit_sha: Option<String>,
    pub error: Option<String>,
}

/// A project with a repository and at least one tracked branch.
pub struct Tracked {
    pub id: String,
    pub team_id: String,
    pub repo: String,
}

/// A branch whose map is behind `head_sha`.
pub struct Waiting {
    pub project_id: String,
    pub team_id: String,
    pub repo: String,
    pub name: String,
    pub head_sha: String,
}

/// A branch's map.
#[derive(Serialize)]
pub struct Map {
    /// The commit it describes; `None` until the branch is first mapped.
    pub commit: Option<String>,
    pub files: Vec<MapFile>,
    /// Pairs of a file and a file it uses.
    pub uses: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct MapFile {
    pub path: String,
    pub language: Option<&'static str>,
    pub lines: i64,
    pub symbols: Vec<Symbol>,
}

/// A team's own GitHub App.
#[derive(Deserialize)]
pub struct GithubApp {
    pub id: i64,
    pub slug: String,
    /// The private key that signs the app's tokens.
    pub pem: String,
}

/// What the person opening an invite link sees.
#[derive(Serialize)]
pub struct Invitation {
    pub team_name: String,
    pub invited_by: String,
    pub email: String,
    /// Tells the web app to offer signing in, not registering.
    pub has_account: bool,
}

pub async fn has_users(pool: &SqlitePool) -> sqlx::Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM users) AS "found: bool""#)
        .fetch_one(pool)
        .await
}

pub async fn user_exists(pool: &SqlitePool, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users WHERE email = ?) AS "found: bool""#,
        email
    )
    .fetch_one(pool)
    .await
}

/// The user of a session renewed after `since`.
pub async fn user_by_session(
    pool: &SqlitePool,
    token: &str,
    since: i64,
) -> sqlx::Result<Option<User>> {
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
        Team,
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

/// False when the address asked for a code less than a minute ago; that code stands.
/// Wrong tries stay counted while the previous code is live, so asking again buys no guesses.
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

/// Lets a code be asked for again at once, for when its mail couldn't be sent.
pub async fn forget_code_wait(pool: &SqlitePool, email: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE sign_in_codes SET expires_at = expires_at - ? WHERE email = ?",
        CODE_WAIT,
        email
    )
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
        (None, Some(name)) => create_user(&mut tx, email, &name, now).await?,
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

async fn create_user(
    tx: &mut sqlx::SqliteConnection,
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

/// Starts the session's year again, unless that was done after `before`. Returns whether it was.
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

pub async fn rename_team(pool: &SqlitePool, team_id: &str, name: &str) -> sqlx::Result<()> {
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

pub async fn is_member(pool: &SqlitePool, team_id: &str, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM team_members m JOIN users u ON u.id = m.user_id
                          WHERE m.team_id = ? AND u.email = ?) AS "found: bool""#,
        team_id,
        email
    )
    .fetch_one(pool)
    .await
}

/// Returns whether a member was removed. The team's owner never is.
pub async fn remove_member(pool: &SqlitePool, team_id: &str, user_id: &str) -> sqlx::Result<bool> {
    let removed = sqlx::query!(
        "DELETE FROM team_members WHERE team_id = ? AND user_id = ? AND role = 'member'",
        team_id,
        user_id
    )
    .execute(pool)
    .await?;
    Ok(removed.rows_affected() > 0)
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

/// Inviting an address again replaces its token and expiry.
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

/// Uses the invite up. Returns the team joined, or `None` when the token is gone.
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

pub async fn projects(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Vec<Project>> {
    let projects = sqlx::query!(
        "SELECT id, name, repo FROM projects WHERE team_id = ? ORDER BY name",
        team_id
    )
    .fetch_all(pool)
    .await?;
    let mut branches = sqlx::query_as!(
        Branch,
        "SELECT b.project_id, b.name, b.head_sha, b.commit_sha, b.error
         FROM branches b JOIN projects p ON p.id = b.project_id
         WHERE p.team_id = ? ORDER BY b.name",
        team_id
    )
    .fetch_all(pool)
    .await?;
    let projects = projects.into_iter().map(|project| Project {
        branches: branches
            .extract_if(.., |branch| branch.project_id == project.id)
            .collect(),
        id: project.id,
        name: project.name,
        repo: project.repo,
    });
    Ok(projects.collect())
}

pub async fn create_project(
    pool: &SqlitePool,
    team_id: &str,
    name: &str,
    repo: Option<&str>,
    branches: &[String],
    now: i64,
) -> sqlx::Result<String> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    sqlx::query!(
        "INSERT INTO projects (id, team_id, name, repo, created_at) VALUES (?, ?, ?, ?, ?)",
        id,
        team_id,
        name,
        repo,
        now
    )
    .execute(&mut *tx)
    .await?;
    track_branches(&mut tx, &id, branches, false).await?;
    tx.commit().await?;
    Ok(id)
}

/// Whether the tracked branches changed; `None` when the team has no such project.
pub async fn update_project(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    name: &str,
    repo: Option<&str>,
    branches: &[String],
) -> sqlx::Result<Option<bool>> {
    // It reads before it writes, and SQLite refuses the write if another landed in between.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let old_repo = sqlx::query_scalar!(
        "SELECT repo FROM projects WHERE team_id = ? AND id = ?",
        team_id,
        project_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(old_repo) = old_repo else {
        return Ok(None);
    };
    sqlx::query!(
        "UPDATE projects SET name = ?, repo = ? WHERE id = ?",
        name,
        repo,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    // Another repository's maps and docs say nothing about this one.
    let moved = old_repo.as_deref() != repo;
    if moved {
        sqlx::query!("DELETE FROM docs WHERE project_id = ?", project_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query!(
            "UPDATE projects SET docs_state = 'idle', docs_message = NULL, docs_run = NULL,
                    docs_branch = NULL, docs_commit = NULL, docs_left_out = NULL
             WHERE id = ?",
            project_id
        )
        .execute(&mut *tx)
        .await?;
    }
    let changed = track_branches(&mut tx, project_id, branches, moved).await?;
    tx.commit().await?;
    Ok(Some(changed))
}

pub async fn delete_project(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM projects WHERE team_id = ? AND id = ?",
        team_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Makes `names` the project's tracked branches, and returns whether that changed anything.
/// `reset` forgets every map first.
async fn track_branches(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    names: &[String],
    reset: bool,
) -> sqlx::Result<bool> {
    let mut changed = false;
    for name in branch_names(&mut *tx, project_id).await? {
        if reset || !names.contains(&name) {
            sqlx::query!(
                "DELETE FROM branches WHERE project_id = ? AND name = ?",
                project_id,
                name
            )
            .execute(&mut *tx)
            .await?;
            changed = true;
        }
    }
    for name in names {
        let added = sqlx::query!(
            "INSERT OR IGNORE INTO branches (project_id, name) VALUES (?, ?)",
            project_id,
            name
        )
        .execute(&mut *tx)
        .await?;
        changed |= added.rows_affected() > 0;
    }
    forget_unlisted_blobs(tx, project_id).await?;
    Ok(changed)
}

/// Deletes the contents that no branch of the project lists any more, with what was read from them.
async fn forget_unlisted_blobs(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM blobs WHERE project_id = ?1
         AND blob NOT IN (SELECT blob FROM branch_files WHERE project_id = ?1)",
        project_id
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn branch_names(
    db: impl sqlx::SqliteExecutor<'_>,
    project_id: &str,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!("SELECT name FROM branches WHERE project_id = ?", project_id)
        .fetch_all(db)
        .await
}

pub async fn tracked_projects(pool: &SqlitePool) -> sqlx::Result<Vec<Tracked>> {
    sqlx::query_as!(
        Tracked,
        r#"SELECT id, team_id, repo AS "repo!" FROM projects
           WHERE repo IS NOT NULL AND id IN (SELECT project_id FROM branches)"#
    )
    .fetch_all(pool)
    .await
}

/// Records the branch's latest commit on GitHub, or why it couldn't be learned or mapped.
pub async fn set_branch_head(
    pool: &SqlitePool,
    project_id: &str,
    name: &str,
    head_sha: Option<&str>,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE branches SET head_sha = COALESCE(?1, head_sha), error = ?2
         WHERE project_id = ?3 AND name = ?4
           AND (head_sha IS NOT COALESCE(?1, head_sha) OR error IS NOT ?2)",
        head_sha,
        error,
        project_id,
        name
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn waiting_branches(pool: &SqlitePool) -> sqlx::Result<Vec<Waiting>> {
    sqlx::query_as!(
        Waiting,
        r#"SELECT b.project_id, p.team_id, p.repo AS "repo!", b.name, b.head_sha AS "head_sha!"
           FROM branches b JOIN projects p ON p.id = b.project_id
           WHERE b.head_sha IS NOT NULL AND b.head_sha IS NOT b.commit_sha AND b.error IS NULL"#
    )
    .fetch_all(pool)
    .await
}

/// Forgets every map made by another version of the reader, so that all are read again.
pub async fn forget_maps_of_other_readers(pool: &SqlitePool, reader: i64) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar!("SELECT map_reader FROM config")
        .fetch_one(&mut *tx)
        .await?;
    if current != reader {
        sqlx::query!("DELETE FROM branch_files")
            .execute(&mut *tx)
            .await?;
        sqlx::query!("DELETE FROM blobs").execute(&mut *tx).await?;
        sqlx::query!("UPDATE branches SET commit_sha = NULL")
            .execute(&mut *tx)
            .await?;
        sqlx::query!("UPDATE config SET map_reader = ?", reader)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

pub async fn known_blobs(pool: &SqlitePool, project_id: &str) -> sqlx::Result<HashSet<String>> {
    let blobs = sqlx::query_scalar!("SELECT blob FROM blobs WHERE project_id = ?", project_id)
        .fetch_all(pool)
        .await?;
    Ok(blobs.into_iter().collect())
}

/// Saves what was newly read, and makes `files` the branch's map at its latest commit.
/// Does nothing when the branch or its project changed while the commit was being read.
pub async fn save_map(
    pool: &SqlitePool,
    branch: &Waiting,
    files: &[(String, String)],
    readings: &[(String, Reading)],
) -> sqlx::Result<()> {
    let project = &branch.project_id;
    let mut tx = pool.begin().await?;
    let current = sqlx::query!(
        "UPDATE branches SET commit_sha = ?1
         WHERE project_id = ?2 AND name = ?3 AND head_sha = ?1
           AND EXISTS (SELECT 1 FROM projects WHERE id = ?2 AND repo = ?4)",
        branch.head_sha,
        project,
        branch.name,
        branch.repo
    )
    .execute(&mut *tx)
    .await?;
    if current.rows_affected() == 0 {
        return Ok(());
    }
    for (blob, reading) in readings {
        sqlx::query!(
            "INSERT OR IGNORE INTO blobs (project_id, blob, lines, text) VALUES (?, ?, ?, ?)",
            project,
            blob,
            reading.lines,
            reading.text
        )
        .execute(&mut *tx)
        .await?;
        for symbol in &reading.symbols {
            sqlx::query!(
                "INSERT INTO blob_symbols (project_id, blob, name, kind, line, end_line)
                 VALUES (?, ?, ?, ?, ?, ?)",
                project,
                blob,
                symbol.name,
                symbol.kind,
                symbol.line,
                symbol.end_line
            )
            .execute(&mut *tx)
            .await?;
        }
        for name in &reading.mentions {
            sqlx::query!(
                "INSERT OR IGNORE INTO blob_mentions (project_id, blob, name) VALUES (?, ?, ?)",
                project,
                blob,
                name
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query!(
        "DELETE FROM branch_files WHERE project_id = ? AND branch = ?",
        project,
        branch.name
    )
    .execute(&mut *tx)
    .await?;
    for (path, blob) in files {
        sqlx::query!(
            "INSERT INTO branch_files (project_id, branch, path, blob) VALUES (?, ?, ?, ?)",
            project,
            branch.name,
            path,
            blob
        )
        .execute(&mut *tx)
        .await?;
    }
    // A content known from another branch is gone if that branch was untracked meanwhile.
    // Nothing is saved then, so the branch still waits and is mapped again.
    let incomplete = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM branch_files f
               WHERE f.project_id = ?1 AND f.branch = ?2 AND NOT EXISTS (
                   SELECT 1 FROM blobs b WHERE b.project_id = f.project_id AND b.blob = f.blob)
           ) AS "found: bool""#,
        project,
        branch.name
    )
    .fetch_one(&mut *tx)
    .await?;
    if incomplete {
        return Ok(());
    }
    forget_unlisted_blobs(&mut tx, project).await?;
    tx.commit().await
}

/// The commit a branch's map describes, itself `None` until the branch is first mapped.
/// `None` when the team has no such project or the project doesn't track the branch.
pub async fn branch_commit(
    db: impl sqlx::SqliteExecutor<'_>,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<Option<String>>> {
    sqlx::query_scalar!(
        "SELECT b.commit_sha FROM branches b JOIN projects p ON p.id = b.project_id
         WHERE p.team_id = ? AND b.project_id = ? AND b.name = ?",
        team_id,
        project_id,
        branch
    )
    .fetch_optional(db)
    .await
}

/// `None` as for `branch_commit`.
pub async fn map(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<Map>> {
    // One transaction, so that a map saved meanwhile can't mix into this one.
    let mut tx = pool.begin().await?;
    read_map(&mut tx, team_id, project_id, branch).await
}

/// A branch's map with the text of its text files by path, both as they were at one moment.
pub async fn sources(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<(Map, HashMap<String, String>)>> {
    let mut tx = pool.begin().await?;
    let Some(map) = read_map(&mut tx, team_id, project_id, branch).await? else {
        return Ok(None);
    };
    let texts = sqlx::query!(
        r#"SELECT f.path, b.text AS "text!"
           FROM branch_files f JOIN blobs b ON b.project_id = f.project_id AND b.blob = f.blob
           WHERE f.project_id = ? AND f.branch = ? AND b.text IS NOT NULL"#,
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;
    let texts = texts.into_iter().map(|row| (row.path, row.text)).collect();
    Ok(Some((map, texts)))
}

async fn read_map(
    tx: &mut sqlx::SqliteConnection,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<Map>> {
    let Some(commit) = branch_commit(&mut *tx, team_id, project_id, branch).await? else {
        return Ok(None);
    };

    let files = sqlx::query!(
        "SELECT f.path, b.lines
         FROM branch_files f JOIN blobs b ON b.project_id = f.project_id AND b.blob = f.blob
         WHERE f.project_id = ? AND f.branch = ? ORDER BY f.path",
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;
    let symbols = sqlx::query!(
        "SELECT f.path, s.name, s.kind, s.line, s.end_line
         FROM branch_files f JOIN blob_symbols s ON s.project_id = f.project_id AND s.blob = f.blob
         WHERE f.project_id = ? AND f.branch = ? ORDER BY s.line",
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut by_path: HashMap<String, Vec<Symbol>> = HashMap::new();
    for row in symbols {
        let symbol = Symbol {
            name: row.name,
            kind: row.kind,
            line: row.line,
            end_line: row.end_line,
        };
        by_path.entry(row.path).or_default().push(symbol);
    }
    let files = files
        .into_iter()
        .map(|file| MapFile {
            symbols: by_path.remove(&file.path).unwrap_or_default(),
            language: crate::map::language(&file.path).map(|language| language.name),
            path: file.path,
            lines: file.lines,
        })
        .collect();

    // A file uses another when it mentions a name that only that other file defines.
    let uses = sqlx::query!(
        r#"WITH defined AS (
               SELECT s.name, MIN(f.path) AS path
               FROM branch_files f
               JOIN blob_symbols s ON s.project_id = f.project_id AND s.blob = f.blob
               WHERE f.project_id = ?1 AND f.branch = ?2
               GROUP BY s.name HAVING COUNT(DISTINCT f.path) = 1
           )
           SELECT DISTINCT f.path AS user, defined.path AS "used!"
           FROM branch_files f
           JOIN blob_mentions m ON m.project_id = f.project_id AND m.blob = f.blob
           JOIN defined ON defined.name = m.name
           WHERE f.project_id = ?1 AND f.branch = ?2 AND f.path != defined.path
           ORDER BY 1, 2"#,
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(Some(Map {
        commit,
        files,
        uses: uses.into_iter().map(|row| (row.user, row.used)).collect(),
    }))
}

pub async fn writer(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Option<Writer>> {
    sqlx::query_as!(
        Writer,
        r#"SELECT writer AS "kind!: _", writer_key AS key, writer_model AS model
           FROM teams WHERE id = ? AND writer IS NOT NULL"#,
        team_id
    )
    .fetch_optional(pool)
    .await
}

/// `None` forgets the team's writer.
pub async fn save_writer(
    pool: &SqlitePool,
    team_id: &str,
    writer: Option<&Writer>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE teams SET writer = ?, writer_key = ?, writer_model = ? WHERE id = ?",
        writer.map(|writer| writer.kind),
        writer.and_then(|writer| writer.key.as_ref()),
        writer.and_then(|writer| writer.model.as_ref()),
        team_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Where a project's docs stand, and what they were written from.
pub struct DocsStatus {
    /// `idle`, `running` or `failed`.
    pub state: String,
    pub message: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    /// A JSON list of folders.
    pub left_out: Option<String>,
}

#[derive(Serialize)]
pub struct Doc {
    pub id: String,
    pub section: String,
    pub title: String,
    pub markdown: String,
}

/// `None` when the team has no such project.
pub async fn docs_status(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
) -> sqlx::Result<Option<DocsStatus>> {
    sqlx::query_as!(
        DocsStatus,
        r#"SELECT docs_state AS state, docs_message AS message, docs_branch AS branch,
                  docs_commit AS "commit", docs_left_out AS left_out
           FROM projects WHERE team_id = ? AND id = ?"#,
        team_id,
        project_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn docs(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Doc>> {
    sqlx::query_as!(
        Doc,
        "SELECT id, section, title, markdown FROM docs WHERE project_id = ? ORDER BY position",
        project_id
    )
    .fetch_all(pool)
    .await
}

/// Marks the project's docs as being written by `run`. False when a run already holds it.
pub async fn claim_docs_run(pool: &SqlitePool, project_id: &str, run: i64) -> sqlx::Result<bool> {
    let claimed = sqlx::query!(
        "UPDATE projects SET docs_state = 'running', docs_message = NULL, docs_run = ?
         WHERE id = ? AND docs_state != 'running'",
        run,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(claimed.rows_affected() > 0)
}

/// False, and nothing changes, when `run` no longer holds the project.
pub async fn set_docs_state(
    pool: &SqlitePool,
    project_id: &str,
    run: i64,
    state: &str,
    message: Option<&str>,
) -> sqlx::Result<bool> {
    let set = sqlx::query!(
        "UPDATE projects SET docs_state = ?, docs_message = ? WHERE id = ? AND docs_run = ?",
        state,
        message,
        project_id,
        run
    )
    .execute(pool)
    .await?;
    Ok(set.rows_affected() > 0)
}

/// A run can't outlive the server, so one still marked as running was cut short.
pub async fn fail_interrupted_docs(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE projects SET docs_state = 'failed', docs_message = 'The server restarted'
         WHERE docs_state = 'running'"
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Replaces the project's docs with `pages`, each a section, a title and its Markdown.
/// Does nothing unless `run` still holds the project.
pub async fn save_docs(
    pool: &SqlitePool,
    project_id: &str,
    run: i64,
    branch: &str,
    commit: &str,
    left_out: &str,
    pages: &[(&str, String, String)],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let held = sqlx::query!(
        "UPDATE projects SET docs_state = 'idle', docs_message = NULL, docs_branch = ?,
                             docs_commit = ?, docs_left_out = ?
         WHERE id = ? AND docs_run = ?",
        branch,
        commit,
        left_out,
        project_id,
        run
    )
    .execute(&mut *tx)
    .await?;
    if held.rows_affected() == 0 {
        return Ok(());
    }
    sqlx::query!("DELETE FROM docs WHERE project_id = ?", project_id)
        .execute(&mut *tx)
        .await?;
    for (position, (section, title, markdown)) in (0_i64..).zip(pages) {
        let id = new_id();
        sqlx::query!(
            "INSERT INTO docs (id, project_id, section, position, title, markdown)
             VALUES (?, ?, ?, ?, ?, ?)",
            id,
            project_id,
            section,
            position,
            title,
            markdown
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn github_app(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Option<GithubApp>> {
    sqlx::query_as!(
        GithubApp,
        r#"SELECT github_app_id AS "id!", github_app_slug AS "slug!", github_private_key AS "pem!"
           FROM teams WHERE id = ? AND github_app_id IS NOT NULL"#,
        team_id
    )
    .fetch_optional(pool)
    .await
}

/// `None` forgets the team's app.
pub async fn save_github_app(
    pool: &SqlitePool,
    team_id: &str,
    app: Option<&GithubApp>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE teams SET github_app_id = ?, github_app_slug = ?, github_private_key = ?
         WHERE id = ?",
        app.map(|app| app.id),
        app.map(|app| &app.slug),
        app.map(|app| &app.pem),
        team_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn smtp(pool: &SqlitePool) -> sqlx::Result<Option<Smtp>> {
    sqlx::query_as!(
        Smtp,
        r#"SELECT smtp_host AS "host!", smtp_port AS "port!: _", smtp_security AS "security!: _",
                  smtp_username AS username, smtp_password AS password, mail_from AS "from!"
           FROM config WHERE smtp_host IS NOT NULL"#
    )
    .fetch_optional(pool)
    .await
}

/// `None` clears the SMTP settings.
pub async fn save_smtp(pool: &SqlitePool, smtp: Option<&Smtp>) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE config SET smtp_host = ?, smtp_port = ?, smtp_security = ?,
                           smtp_username = ?, smtp_password = ?, mail_from = ?",
        smtp.map(|smtp| &smtp.host),
        smtp.map(|smtp| smtp.port),
        smtp.map(|smtp| smtp.security),
        smtp.and_then(|smtp| smtp.username.as_ref()),
        smtp.and_then(|smtp| smtp.password.as_ref()),
        smtp.map(|smtp| &smtp.from)
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Serialize)]
pub struct Share {
    pub token: String,
    /// The name of who made it.
    pub created_by: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct Shared {
    pub title: String,
    pub created_at: i64,
    pub pages: Vec<SharedPage>,
}

#[derive(Serialize)]
pub struct SharedPage {
    pub title: String,
    pub markdown: String,
}

/// Keeps `pages`, each a title and its Markdown, behind `token`. False when the team has no such project.
pub async fn share_docs(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    token: &str,
    user_id: &str,
    now: i64,
    pages: &[(&str, &str)],
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let shared = sqlx::query!(
        "INSERT INTO shares (token, project_id, title, created_by, created_at)
         SELECT ?, id, name, ?, ? FROM projects WHERE team_id = ? AND id = ?",
        token,
        user_id,
        now,
        team_id,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    if shared.rows_affected() == 0 {
        return Ok(false);
    }
    for (position, (title, markdown)) in (0i64..).zip(pages) {
        sqlx::query!(
            "INSERT INTO share_pages (token, position, title, markdown) VALUES (?, ?, ?, ?)",
            token,
            position,
            title,
            markdown
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

pub async fn shares(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
) -> sqlx::Result<Vec<Share>> {
    sqlx::query_as!(
        Share,
        "SELECT s.token, u.name AS created_by, s.created_at
         FROM shares s JOIN projects p ON p.id = s.project_id JOIN users u ON u.id = s.created_by
         WHERE p.team_id = ? AND s.project_id = ?
         ORDER BY s.created_at DESC",
        team_id,
        project_id
    )
    .fetch_all(pool)
    .await
}

/// Returns whether the project had such a share.
pub async fn end_share(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    token: &str,
) -> sqlx::Result<bool> {
    let ended = sqlx::query!(
        "DELETE FROM shares WHERE token = ? AND project_id =
             (SELECT id FROM projects WHERE team_id = ? AND id = ?)",
        token,
        team_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(ended.rows_affected() > 0)
}

pub async fn shared(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<Shared>> {
    let share = sqlx::query!(
        "SELECT title, created_at FROM shares WHERE token = ?",
        token
    )
    .fetch_optional(pool)
    .await?;
    let Some(share) = share else {
        return Ok(None);
    };
    let pages = sqlx::query_as!(
        SharedPage,
        "SELECT title, markdown FROM share_pages WHERE token = ? ORDER BY position",
        token
    )
    .fetch_all(pool)
    .await?;
    Ok(Some(Shared {
        title: share.title,
        created_at: share.created_at,
        pages,
    }))
}
