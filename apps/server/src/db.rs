use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::types::Json;

use crate::chats::Start;
use crate::mail::Smtp;
use crate::writer::Writer;
use code_map::{Reading, Symbol};

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
    /// What its tasks are named by, as `SF` in SF-12.
    pub key: String,
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
        "SELECT id, name, key, repo FROM projects WHERE team_id = ? ORDER BY name",
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
        key: project.key,
        repo: project.repo,
    });
    Ok(projects.collect())
}

pub async fn create_project(
    pool: &SqlitePool,
    team_id: &str,
    name: &str,
    key: &str,
    repo: Option<&str>,
    branches: &[String],
    now: i64,
) -> sqlx::Result<String> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    sqlx::query!(
        "INSERT INTO projects (id, team_id, name, key, repo, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        id,
        team_id,
        name,
        key,
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
    key: &str,
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
        "UPDATE projects SET name = ?, key = ?, repo = ? WHERE id = ?",
        name,
        key,
        repo,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    // Another repository's maps say nothing about this one. The pages stay, since people wrote
    // in them, without what tied them to the old repository.
    let moved = old_repo.as_deref() != repo;
    if moved {
        sqlx::query!(
            "UPDATE docs SET files = '[]', unread = '[]', commit_sha = NULL, notes = NULL, checking = 0
             WHERE project_id = ?",
            project_id
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!("DELETE FROM proposals WHERE project_id = ?", project_id)
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
            "INSERT OR IGNORE INTO blobs (project_id, blob, lines, text, scope)
             VALUES (?, ?, ?, ?, ?)",
            project,
            blob,
            reading.lines,
            reading.text,
            reading.scope
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
        for import in &reading.imports {
            sqlx::query!(
                "INSERT OR IGNORE INTO blob_imports (project_id, blob, import) VALUES (?, ?, ?)",
                project,
                blob,
                import
            )
            .execute(&mut *tx)
            .await?;
        }
        for (name, member) in &reading.mentions {
            sqlx::query!(
                "INSERT OR IGNORE INTO blob_mentions (project_id, blob, name, member)
                 VALUES (?, ?, ?, ?)",
                project,
                blob,
                name,
                member
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
    let Some((commit, files)) = read_files(&mut tx, team_id, project_id, branch).await? else {
        return Ok(None);
    };
    let uses = uses(&mut tx, project_id, branch, &files).await?;
    Ok(Some(Map {
        commit,
        files,
        uses,
    }))
}

/// A branch's commit, its files with their symbols, and the text of its text files by path,
/// all as they were at one moment. Which file uses which is left out: the docs writer has no use for it.
pub async fn sources(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<(Option<String>, Vec<MapFile>, HashMap<String, String>)>> {
    let mut tx = pool.begin().await?;
    let Some((commit, files)) = read_files(&mut tx, team_id, project_id, branch).await? else {
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
    Ok(Some((commit, files, texts)))
}

/// A branch's commit and its files with their symbols, in the order of their paths.
async fn read_files(
    tx: &mut sqlx::SqliteConnection,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<(Option<String>, Vec<MapFile>)>> {
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
            language: code_map::language(&file.path).map(|language| language.name),
            path: file.path,
            lines: file.lines,
        })
        .collect();
    Ok(Some((commit, files)))
}

/// The pairs of a file and a file it uses. It depends on where each file sits, so it is
/// worked out when asked for, not kept.
async fn uses(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    branch: &str,
    files: &[MapFile],
) -> sqlx::Result<Vec<(String, String)>> {
    let scopes = sqlx::query!(
        r#"SELECT f.path, b.scope AS "scope!"
           FROM branch_files f JOIN blobs b ON b.project_id = f.project_id AND b.blob = f.blob
           WHERE f.project_id = ? AND f.branch = ? AND b.scope IS NOT NULL"#,
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;
    let imports = sqlx::query!(
        "SELECT f.path, i.import
         FROM branch_files f JOIN blob_imports i ON i.project_id = f.project_id AND i.blob = f.blob
         WHERE f.project_id = ? AND f.branch = ?",
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;
    let mentions = sqlx::query!(
        r#"SELECT f.path, m.name, m.member AS "member: bool"
           FROM branch_files f JOIN blob_mentions m ON m.project_id = f.project_id AND m.blob = f.blob
           WHERE f.project_id = ? AND f.branch = ?"#,
        project_id,
        branch
    )
    .fetch_all(&mut *tx)
    .await?;

    let place: HashMap<&str, usize> = (files.iter().map(|file| &*file.path)).zip(0..).collect();
    let mut read: Vec<code_map::File> = files
        .iter()
        .map(|file| code_map::File {
            path: file.path.clone(),
            scope: None,
            names: (file.symbols.iter().map(|symbol| symbol.name.clone())).collect(),
            imports: Vec::new(),
            mentions: Vec::new(),
        })
        .collect();
    for row in scopes {
        read[place[&*row.path]].scope = Some(row.scope);
    }
    for row in imports {
        read[place[&*row.path]].imports.push(row.import);
    }
    for row in mentions {
        let mention = (row.name, row.member);
        read[place[&*row.path]].mentions.push(mention);
    }
    let uses = code_map::link(&read).into_iter();
    let path = |at: usize| files[at].path.clone();
    Ok(uses.map(|(user, used)| (path(user), path(used))).collect())
}

pub async fn writer(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Option<Writer>> {
    sqlx::query_as!(
        Writer,
        r#"SELECT writer AS "kind!: _", writer_key AS key, writer_model AS model, '' AS "asker!"
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

/// Writes a column that holds a JSON list as that list, not as a string of it.
fn as_list<S: serde::Serializer>(json: &str, to: S) -> Result<S::Ok, S::Error> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(serde::ser::Error::custom)?;
    value.serialize(to)
}

/// The same for a column that may hold nothing.
fn as_json<S: serde::Serializer>(json: &Option<String>, to: S) -> Result<S::Ok, S::Error> {
    as_list(json.as_deref().unwrap_or("null"), to)
}

#[derive(Serialize)]
pub struct Doc {
    pub id: String,
    pub section: String,
    pub title: String,
    pub markdown: String,
    /// The id of its newest version, which an edit names as what it started from.
    pub version: String,
    /// JSON lists: the files it was written from, and those the writer had no room to read.
    #[serde(serialize_with = "as_list")]
    pub files: String,
    #[serde(serialize_with = "as_list")]
    pub unread: String,
    /// The commit its text was written from, which its citations point into.
    pub commit: Option<String>,
    /// The text the docs writer last gave it, which tells a person's words from the writer's own.
    #[serde(skip)]
    pub written: Option<String>,
    /// A JSON list of the writer's notes on a person's words, once it has checked them.
    #[serde(serialize_with = "as_json")]
    pub notes: Option<String>,
    pub checking: bool,
}

/// A page as a run wrote it, or a change to one.
pub struct Written {
    /// The page to change or, without `markdown`, to remove. `None` for a new page.
    pub doc: Option<(String, String)>,
    pub section: String,
    pub title: String,
    pub markdown: Option<String>,
    /// JSON lists.
    pub files: String,
    pub unread: String,
    pub notes: String,
    pub reason: String,
}

impl Written {
    fn text<'a>(&'a self, markdown: &'a str, commit: Option<&'a str>) -> Text<'a> {
        Text {
            section: &self.section,
            title: &self.title,
            markdown,
            files: &self.files,
            unread: &self.unread,
            commit,
        }
    }
}

/// A page's text with what it was written from.
struct Text<'a> {
    section: &'a str,
    title: &'a str,
    markdown: &'a str,
    files: &'a str,
    unread: &'a str,
    commit: Option<&'a str>,
}

/// A change the writer suggests.
#[derive(Serialize)]
pub struct Proposal {
    pub id: String,
    /// The page it changes or removes, or `None` for a new page.
    pub page: Option<String>,
    /// The page's version it was written against.
    pub base: Option<String>,
    pub section: String,
    pub title: String,
    /// `None` to remove the page.
    pub markdown: Option<String>,
    #[serde(serialize_with = "as_json")]
    pub notes: Option<String>,
    /// A JSON list of the files the writer had no room to read.
    #[serde(serialize_with = "as_list")]
    pub unread: String,
    pub reason: String,
    pub request: Option<String>,
}

#[derive(Serialize)]
pub struct DocVersion {
    pub id: String,
    pub title: String,
    pub markdown: String,
    /// The name of who wrote it, or `None` for the docs writer.
    pub author: Option<String>,
    pub created_at: i64,
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
        r#"SELECT d.id, d.section, d.title, d.markdown, d.files, d.unread, d.notes,
                  d.commit_sha AS "commit", d.checking AS "checking: bool",
                  (SELECT MAX(v.id) FROM doc_versions v WHERE v.doc_id = d.id) AS "version!",
                  (SELECT v.markdown FROM doc_versions v
                   WHERE v.doc_id = d.id AND v.author IS NULL
                   ORDER BY v.id DESC LIMIT 1) AS written
           FROM docs d WHERE d.project_id = ? ORDER BY d.position"#,
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
    sqlx::query!("UPDATE docs SET checking = 0 WHERE checking")
        .execute(pool)
        .await?;
    Ok(())
}

/// Ends `run` with what it wrote, and does nothing unless it still holds the project.
/// `commit` is what it was all written from. `from` is the branch and the left out folders of
/// a run over the whole project, which drops the proposals nobody answered; a request has none. A project without pages gets
/// the pages themselves, and one with pages gets them as proposals.
pub async fn save_written(
    pool: &SqlitePool,
    project_id: &str,
    run: i64,
    commit: &str,
    from: Option<(&str, &str)>,
    request: Option<&str>,
    written: &[Written],
) -> sqlx::Result<()> {
    let (branch, left_out) = from.unzip();
    let ran = from.map(|_| commit);
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let held = sqlx::query!(
        "UPDATE projects SET docs_state = 'idle', docs_message = NULL,
                docs_branch = COALESCE(?, docs_branch), docs_commit = COALESCE(?, docs_commit),
                docs_left_out = COALESCE(?, docs_left_out)
         WHERE id = ? AND docs_run = ?",
        branch,
        ran,
        left_out,
        project_id,
        run
    )
    .execute(&mut *tx)
    .await?;
    if held.rows_affected() == 0 {
        return Ok(());
    }
    if from.is_some() {
        sqlx::query!("DELETE FROM proposals WHERE project_id = ?", project_id)
            .execute(&mut *tx)
            .await?;
    }
    let pages = sqlx::query_scalar!("SELECT COUNT(*) FROM docs WHERE project_id = ?", project_id)
        .fetch_one(&mut *tx)
        .await?;
    for page in written {
        if pages == 0 && from.is_some() {
            let text = page.markdown.as_deref().unwrap_or_default();
            add_page(&mut tx, project_id, &page.text(text, Some(commit)), None).await?;
            continue;
        }
        let id = new_id();
        let (doc_id, base) = page.doc.clone().unzip();
        sqlx::query!(
            "INSERT INTO proposals (id, project_id, doc_id, base, section, title, markdown, files,
                                    unread, commit_sha, notes, reason, request, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch())",
            id,
            project_id,
            doc_id,
            base,
            page.section,
            page.title,
            page.markdown,
            page.files,
            page.unread,
            commit,
            page.notes,
            page.reason,
            request
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn proposals(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Proposal>> {
    sqlx::query_as!(
        Proposal,
        r#"SELECT id, doc_id AS page, base, section, title, markdown, notes AS "notes?",
                  unread, reason, request
           FROM proposals WHERE project_id = ? ORDER BY id"#,
        project_id
    )
    .fetch_all(pool)
    .await
}

/// Does what a proposal suggests and deletes it. `seen` is the version of its page that the
/// person accepting it looked at. `markdown` is the text they made of the proposal's, saved
/// under their name; without it the proposal's own is saved as the writer's.
/// `None` when the team's project has no such proposal, false when the page has changed
/// since `seen`, which leaves the proposal as it was.
pub async fn accept_proposal(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    id: &str,
    seen: Option<&str>,
    markdown: Option<&str>,
    user_id: &str,
) -> sqlx::Result<Option<bool>> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let proposal = sqlx::query!(
        "DELETE FROM proposals
         WHERE id = ? AND project_id = (SELECT id FROM projects WHERE team_id = ? AND id = ?)
         RETURNING doc_id, section, title, markdown, files, unread, commit_sha",
        id,
        team_id,
        project_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(proposal) = proposal else {
        return Ok(None);
    };
    if let Some(doc_id) = &proposal.doc_id {
        let current =
            sqlx::query_scalar!("SELECT MAX(id) FROM doc_versions WHERE doc_id = ?", doc_id);
        // Leaving here undoes the deletion above with the rest of the transaction.
        if current.fetch_one(&mut *tx).await?.as_deref() != seen {
            return Ok(Some(false));
        }
    }
    let author = markdown.map(|_| user_id);
    let text = |markdown| Text {
        section: &proposal.section,
        title: &proposal.title,
        markdown,
        files: &proposal.files,
        unread: &proposal.unread,
        commit: proposal.commit_sha.as_deref(),
    };
    match (&proposal.doc_id, markdown.or(proposal.markdown.as_deref())) {
        (None, markdown) => {
            add_page(
                &mut tx,
                project_id,
                &text(markdown.unwrap_or_default()),
                author,
            )
            .await?;
        }
        (Some(doc_id), None) => {
            sqlx::query!("DELETE FROM docs WHERE id = ?", doc_id)
                .execute(&mut *tx)
                .await?;
        }
        (Some(doc_id), Some(markdown)) => {
            let text = text(markdown);
            sqlx::query!(
                "UPDATE docs SET title = ?, markdown = ?, files = ?, unread = ?, commit_sha = ?,
                                 notes = NULL, checking = 0
                 WHERE id = ?",
                text.title,
                text.markdown,
                text.files,
                text.unread,
                text.commit,
                doc_id
            )
            .execute(&mut *tx)
            .await?;
            add_version(&mut tx, doc_id, text.title, text.markdown, author).await?;
        }
    }
    tx.commit().await?;
    Ok(Some(true))
}

pub async fn delete_proposal(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    id: &str,
) -> sqlx::Result<bool> {
    let deleted = sqlx::query!(
        "DELETE FROM proposals
         WHERE id = ? AND project_id = (SELECT id FROM projects WHERE team_id = ? AND id = ?)",
        id,
        team_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected() > 0)
}

/// The branch a page is checked against: the one the docs were last written from, or else
/// the first of the project's branches that is mapped.
pub async fn check_branch(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        r#"SELECT COALESCE(p.docs_branch, MIN(b.name)) AS "branch: String"
           FROM projects p LEFT JOIN branches b ON b.project_id = p.id AND b.commit_sha IS NOT NULL
           WHERE p.id = ?"#,
        project_id
    )
    .fetch_one(pool)
    .await
}

/// Marks a page as being checked against the code.
pub async fn start_check(pool: &SqlitePool, doc_id: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE docs SET checking = 1 WHERE id = ?", doc_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Ends the check of a page's `version` with its notes, or with none when the check failed.
/// Does nothing when the page has been saved again since, which started a check of its own.
pub async fn end_check(
    pool: &SqlitePool,
    doc_id: &str,
    version: &str,
    files: Option<&str>,
    notes: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE docs SET checking = 0, files = COALESCE(?, files), notes = ?
         WHERE id = ? AND (SELECT MAX(v.id) FROM doc_versions v WHERE v.doc_id = docs.id) = ?",
        files,
        notes,
        doc_id,
        version
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Takes the note at `index` off a page. False when the team's project has no such page.
pub async fn dismiss_note(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    doc_id: &str,
    index: i64,
) -> sqlx::Result<bool> {
    let dismissed = sqlx::query!(
        "UPDATE docs SET notes = json_remove(notes, '$[' || ?1 || ']')
         WHERE id = ?2 AND project_id = (SELECT id FROM projects WHERE team_id = ?3 AND id = ?4)",
        index,
        doc_id,
        team_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(dismissed.rows_affected() > 0)
}

/// Adds a page at the end of the project's docs, with its first version.
async fn add_page(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    text: &Text<'_>,
    author: Option<&str>,
) -> sqlx::Result<()> {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO docs (id, project_id, section, position, title, markdown, files, unread,
                           commit_sha)
         VALUES (?1, ?2, ?3,
                 (SELECT COALESCE(MAX(position), -1) + 1 FROM docs WHERE project_id = ?2),
                 ?4, ?5, ?6, ?7, ?8)",
        id,
        project_id,
        text.section,
        text.title,
        text.markdown,
        text.files,
        text.unread,
        text.commit
    )
    .execute(&mut *tx)
    .await?;
    add_version(tx, &id, text.title, text.markdown, author).await
}

async fn add_version(
    tx: &mut sqlx::SqliteConnection,
    doc_id: &str,
    title: &str,
    markdown: &str,
    author: Option<&str>,
) -> sqlx::Result<()> {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO doc_versions (id, doc_id, title, markdown, author, created_at)
         VALUES (?, ?, ?, ?, ?, unixepoch())",
        id,
        doc_id,
        title,
        markdown,
        author
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// Adds an empty page at the end and returns its id. `None` when the team has no such project.
pub async fn add_doc(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    section: &str,
    title: &str,
    user_id: &str,
) -> sqlx::Result<Option<String>> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    let added = sqlx::query!(
        "INSERT INTO docs (id, project_id, section, position, title, markdown)
         SELECT ?1, p.id, ?2, (SELECT COALESCE(MAX(position), -1) + 1 FROM docs WHERE project_id = p.id), ?3, ''
         FROM projects p WHERE p.team_id = ?4 AND p.id = ?5",
        id,
        section,
        title,
        team_id,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    if added.rows_affected() == 0 {
        return Ok(None);
    }
    add_version(&mut tx, &id, title, "", Some(user_id)).await?;
    tx.commit().await?;
    Ok(Some(id))
}

#[derive(Deserialize)]
pub struct DocEdit {
    pub title: String,
    pub markdown: String,
    /// The version the edit started from.
    pub version: String,
}

/// Saves a person's edit as the page's new version. `None` when the project has no such page,
/// false when the page is no longer at the version the edit started from.
pub async fn save_doc(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    doc_id: &str,
    edit: &DocEdit,
    user_id: &str,
) -> sqlx::Result<Option<bool>> {
    let title = edit.title.trim();
    let mut tx = pool.begin().await?;
    let saved = sqlx::query!(
        "UPDATE docs SET title = ?1, markdown = ?2, notes = NULL
         WHERE id = ?3 AND project_id = (SELECT id FROM projects WHERE team_id = ?4 AND id = ?5)
           AND (SELECT MAX(v.id) FROM doc_versions v WHERE v.doc_id = docs.id) = ?6",
        title,
        edit.markdown,
        doc_id,
        team_id,
        project_id,
        edit.version
    )
    .execute(&mut *tx)
    .await?;
    if saved.rows_affected() == 0 {
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM docs d JOIN projects p ON p.id = d.project_id
                   WHERE d.id = ? AND p.id = ? AND p.team_id = ?) AS "found: bool""#,
            doc_id,
            project_id,
            team_id
        )
        .fetch_one(&mut *tx)
        .await?;
        return Ok(exists.then_some(false));
    }
    add_version(&mut tx, doc_id, title, &edit.markdown, Some(user_id)).await?;
    tx.commit().await?;
    Ok(Some(true))
}

/// Returns whether the project had such a page.
pub async fn delete_doc(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    doc_id: &str,
) -> sqlx::Result<bool> {
    let deleted = sqlx::query!(
        "DELETE FROM docs WHERE id = ? AND project_id =
             (SELECT id FROM projects WHERE team_id = ? AND id = ?)",
        doc_id,
        team_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected() > 0)
}

/// Puts the project's pages in the order of `ids`.
pub async fn order_docs(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    ids: &[String],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for (position, id) in (0_i64..).zip(ids) {
        sqlx::query!(
            "UPDATE docs SET position = ? WHERE id = ? AND project_id =
                 (SELECT id FROM projects WHERE team_id = ? AND id = ?)",
            position,
            id,
            team_id,
            project_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// A page's versions, newest first. Empty when the project has no such page.
pub async fn doc_versions(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    doc_id: &str,
) -> sqlx::Result<Vec<DocVersion>> {
    sqlx::query_as!(
        DocVersion,
        r#"SELECT v.id, v.title, v.markdown, u.name AS "author?", v.created_at
           FROM doc_versions v
           JOIN docs d ON d.id = v.doc_id
           JOIN projects p ON p.id = d.project_id
           LEFT JOIN users u ON u.id = v.author
           WHERE v.doc_id = ? AND p.id = ? AND p.team_id = ?
           ORDER BY v.id DESC"#,
        doc_id,
        project_id,
        team_id
    )
    .fetch_all(pool)
    .await
}

/// A file kept for a project's docs.
pub struct File<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
}

/// False when the team has no such project.
pub async fn add_file(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    file: File<'_>,
    user_id: &str,
) -> sqlx::Result<bool> {
    let added = sqlx::query!(
        "INSERT INTO files (id, project_id, kind, name, created_by, created_at)
         SELECT ?, id, ?, ?, ?, unixepoch() FROM projects WHERE team_id = ? AND id = ?",
        file.id,
        file.kind,
        file.name,
        user_id,
        team_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(added.rows_affected() > 0)
}

/// A file's media type and name.
pub struct Kept {
    pub kind: String,
    pub name: String,
}

/// One of the project's files.
pub async fn file(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    id: &str,
) -> sqlx::Result<Option<Kept>> {
    sqlx::query_as!(
        Kept,
        "SELECT f.kind, f.name FROM files f JOIN projects p ON p.id = f.project_id
         WHERE f.id = ? AND p.id = ? AND p.team_id = ?",
        id,
        project_id,
        team_id
    )
    .fetch_optional(pool)
    .await
}

/// A file that a page behind the public link `token` shows or links to.
pub async fn shared_file(pool: &SqlitePool, token: &str, id: &str) -> sqlx::Result<Option<Kept>> {
    sqlx::query_as!(
        Kept,
        "SELECT f.kind, f.name FROM files f JOIN shares s ON s.project_id = f.project_id
         WHERE s.token = ?1 AND f.id = ?2 AND EXISTS (
             SELECT 1 FROM share_pages sp
             WHERE sp.token = s.token AND (instr(sp.markdown, 'image:' || f.id) > 0
                 OR instr(sp.markdown, 'file:' || f.id) > 0))",
        token,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Deletes the project and returns the ids of its files, which the caller removes from the folder.
pub async fn delete_project(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
) -> sqlx::Result<Vec<String>> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let files = sqlx::query_scalar!(
        "SELECT f.id FROM files f JOIN projects p ON p.id = f.project_id
         WHERE p.id = ? AND p.team_id = ?",
        project_id,
        team_id
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM projects WHERE team_id = ? AND id = ?",
        team_id,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(files)
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

/// A person's own computer or virtual machine, as its page shows it.
#[derive(Serialize)]
pub struct Machine {
    pub id: String,
    #[serde(skip)]
    pub user_id: String,
    pub name: String,
    pub os: String,
    /// What its core last said of its agents, as a JSON list.
    #[serde(serialize_with = "as_list")]
    pub agents: String,
    pub created_at: i64,
    pub seen_at: Option<i64>,
}

pub async fn machines(pool: &SqlitePool, user_id: &str) -> sqlx::Result<Vec<Machine>> {
    sqlx::query_as!(
        Machine,
        "SELECT id, user_id, name, os, agents, created_at, seen_at
         FROM machines WHERE user_id = ? ORDER BY id",
        user_id
    )
    .fetch_all(pool)
    .await
}

pub async fn machine_by_token(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<Machine>> {
    sqlx::query_as!(
        Machine,
        "SELECT id, user_id, name, os, agents, created_at, seen_at FROM machines WHERE token = ?",
        token
    )
    .fetch_optional(pool)
    .await
}

/// `machine_id` is the machine whose core asked for the code, when one did.
pub async fn add_machine_code(
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

/// Uses up a pairing code made after `since` for a new machine of whoever asked for the code.
/// False when the code is unknown, used or older.
pub async fn pair_machine(
    pool: &SqlitePool,
    code: &str,
    since: i64,
    name: &str,
    os: &str,
    token: &str,
) -> sqlx::Result<bool> {
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
    Ok(asked.is_some())
}

pub async fn delete_machine(pool: &SqlitePool, user_id: &str, id: &str) -> sqlx::Result<bool> {
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
pub async fn see_machine(pool: &SqlitePool, id: &str, agents: Option<&str>) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE machines SET seen_at = unixepoch(), agents = COALESCE(?, agents) WHERE id = ?",
        agents,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A chat as a list shows it.
#[derive(Serialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub agent: String,
    pub branch: String,
    pub running: bool,
    pub project_id: String,
    pub project: String,
    /// Whose it is.
    pub user_id: String,
    pub user: String,
    /// The machine it runs on, unless that was removed.
    pub machine_id: Option<String>,
    pub machine: Option<String>,
    /// The pull request it opened, once it has.
    pub pull_number: Option<i64>,
    pub pull_url: Option<String>,
    /// The hosts its commands may reach.
    pub hosts: Json<Vec<String>>,
    /// The task it is for, if any, and what that is called, as `SF-12 Returns link`.
    pub task_id: Option<String>,
    pub task: Option<String>,
}

/// A team's chats, newest first, or only the one with `id`.
pub async fn chats(pool: &SqlitePool, team_id: &str, id: Option<&str>) -> sqlx::Result<Vec<Chat>> {
    sqlx::query_as!(
        Chat,
        r#"SELECT c.id, c.title, c.agent, c.branch, c.running AS "running: bool", c.project_id,
                  p.name AS project, c.user_id, u.name AS user, c.machine_id, m.name AS "machine?",
                  c.pull_number, c.pull_url, c.hosts AS "hosts: Json<Vec<String>>", c.task_id,
                  p.key || '-' || t.number || ' ' || t.title AS "task?: String"
           FROM projects p JOIN chats c ON c.project_id = p.id JOIN users u ON u.id = c.user_id
           LEFT JOIN machines m ON m.id = c.machine_id LEFT JOIN tasks t ON t.id = c.task_id
           WHERE p.team_id = ?1 AND (?2 IS NULL OR c.id = ?2) ORDER BY c.id DESC"#,
        team_id,
        id
    )
    .fetch_all(pool)
    .await
}

/// What the person wrote is the chat's first event, and its first turn is running.
pub async fn start_chat(
    pool: &SqlitePool,
    project_id: &str,
    user_id: &str,
    agent: &str,
    start: &Start,
) -> sqlx::Result<String> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    let asked = start.text.trim();
    // A chat is called by how it began.
    let title = asked.lines().next().unwrap_or_default();
    let title: String = title.chars().take(60).collect();
    sqlx::query!(
        "INSERT INTO chats (id, project_id, user_id, machine_id, agent, branch, title, asked, task_id, tools_token, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, lower(hex(randomblob(32))), unixepoch())",
        id,
        project_id,
        user_id,
        start.machine_id,
        agent,
        start.branch,
        title,
        asked,
        start.task_id
    )
    .execute(&mut *tx)
    .await?;
    add_chat_event(
        &mut tx,
        &id,
        &serde_json::json!({"kind": "asked", "text": asked}),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn add_chat_event(
    tx: &mut sqlx::SqliteConnection,
    chat_id: &str,
    body: &serde_json::Value,
) -> sqlx::Result<()> {
    let (id, body) = (new_id(), body.to_string());
    sqlx::query!(
        "INSERT INTO chat_events (id, chat_id, body) VALUES (?, ?, ?)",
        id,
        chat_id,
        body
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// Starts a chat's next turn with what its owner wrote. False while a turn is running.
pub async fn ask_chat(pool: &SqlitePool, id: &str, asked: &str) -> sqlx::Result<bool> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let started = sqlx::query!(
        "UPDATE chats SET running = 1, asked = ?, tools_token = lower(hex(randomblob(32)))
         WHERE id = ? AND running = 0",
        asked,
        id
    )
    .execute(&mut *tx)
    .await?;
    if started.rows_affected() == 0 {
        return Ok(false);
    }
    add_chat_event(
        &mut tx,
        id,
        &serde_json::json!({"kind": "asked", "text": asked}),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Takes in the `number`th event a machine tells of a chat of its own, when it is the next
/// one. A turn that ended well gives the session to go on with and the diff of what the
/// chat changed, which is kept once, on the chat, and not with every turn's end.
pub async fn hear_chat(
    pool: &SqlitePool,
    machine_id: &str,
    id: &str,
    number: i64,
    event: &core_link::Event,
) -> sqlx::Result<()> {
    use core_link::Event;
    let (running, session, diff) = match event {
        Event::Done { session, diff, .. } => (false, Some(session), Some(diff)),
        Event::Failed { .. } => (false, None, None),
        Event::Said { .. } | Event::Did { .. } | Event::Wants { .. } => (true, None, None),
    };
    let mut body = serde_json::json!(event);
    body.as_object_mut().map(|body| body.remove("diff"));
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let next = sqlx::query!(
        "UPDATE chats SET heard = heard + 1, running = ?, session = COALESCE(?, session),
                diff = COALESCE(?, diff)
         WHERE id = ? AND machine_id = ? AND heard = ? - 1",
        running,
        session,
        diff,
        id,
        machine_id,
        number
    )
    .execute(&mut *tx)
    .await?;
    if next.rows_affected() == 1 {
        add_chat_event(&mut tx, id, &body).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Ends a chat's turn with why it failed, when the machine can't say so itself.
pub async fn fail_chat(pool: &SqlitePool, id: &str, why: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let ended = sqlx::query!(
        "UPDATE chats SET running = 0 WHERE id = ? AND running = 1",
        id
    )
    .execute(&mut *tx)
    .await?;
    if ended.rows_affected() == 1 {
        add_chat_event(
            &mut tx,
            id,
            &serde_json::json!({"kind": "failed", "why": why}),
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Puts a host among those a chat's commands may reach, and says so in the chat.
pub async fn allow_chat_host(pool: &SqlitePool, id: &str, host: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query!(
        "UPDATE chats SET hosts = (
             SELECT json_group_array(value)
             FROM (SELECT value FROM json_each(chats.hosts) UNION SELECT ?))
         WHERE id = ?",
        host,
        id
    )
    .execute(&mut *tx)
    .await?;
    let allowed = serde_json::json!({"kind": "allowed", "host": host});
    add_chat_event(&mut tx, id, &allowed).await?;
    tx.commit().await
}

/// Takes a host off those a chat's commands may reach.
pub async fn forget_chat_host(pool: &SqlitePool, id: &str, host: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE chats SET hosts = (
             SELECT json_group_array(value) FROM json_each(chats.hosts) WHERE value <> ?)
         WHERE id = ?",
        host,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Everything a chat changed, as git writes it.
pub async fn chat_diff(pool: &SqlitePool, team_id: &str, id: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT c.diff FROM chats c JOIN projects p ON p.id = c.project_id
         WHERE c.id = ? AND p.team_id = ?",
        id,
        team_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn set_chat_pull(
    pool: &SqlitePool,
    id: &str,
    number: i64,
    url: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE chats SET pull_number = ?, pull_url = ? WHERE id = ?",
        number,
        url,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A chat whose turn is running, with what its machine needs to run it or to go on telling of it.
pub struct Running {
    pub id: String,
    pub team_id: String,
    pub agent: String,
    pub branch: String,
    pub asked: String,
    pub session: Option<String>,
    pub heard: i64,
    pub repo: Option<String>,
    pub hosts: Json<Vec<String>>,
    pub tools_token: Option<String>,
}

/// The chats whose turn is running on a machine, or on none any more when `machine_id` is `None`.
pub async fn running_chats(
    pool: &SqlitePool,
    machine_id: Option<&str>,
) -> sqlx::Result<Vec<Running>> {
    sqlx::query_as!(
        Running,
        r#"SELECT c.id, p.team_id, c.agent, c.branch, c.asked, c.session, c.heard, p.repo,
                  c.hosts AS "hosts: Json<Vec<String>>", c.tools_token
           FROM chats c JOIN projects p ON p.id = c.project_id
           WHERE c.machine_id IS ? AND c.running = 1"#,
        machine_id
    )
    .fetch_all(pool)
    .await
}

/// A chat's events after the one with id `after`, as JSON, each with its id.
pub async fn chat_events(
    pool: &SqlitePool,
    chat_id: &str,
    after: &str,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        r#"SELECT json_set(body, '$.id', id) AS "event!: String" FROM chat_events
           WHERE chat_id = ? AND id > ? ORDER BY id"#,
        chat_id,
        after
    )
    .fetch_all(pool)
    .await
}

/// What a task is done when.
#[derive(Serialize, Deserialize)]
pub struct Check {
    pub text: String,
    pub done: bool,
}

#[derive(Serialize)]
pub struct Task {
    pub id: String,
    pub number: i64,
    pub title: String,
    pub text: String,
    pub checks: Json<Vec<Check>>,
    pub status: String,
    /// Who does it, if anybody.
    pub owner_id: Option<String>,
    pub owner: Option<String>,
    /// Who asked for it.
    pub asker_id: Option<String>,
    pub asker: Option<String>,
    pub created_at: i64,
    /// The chat whose agent made it, if one did.
    pub chat_id: Option<String>,
    /// The pages of the project's docs it touches.
    pub docs: Json<Vec<String>>,
}

/// What a task is made from or changed to: only what is given changes.
#[derive(Deserialize)]
pub struct TaskFields {
    pub title: Option<String>,
    pub text: Option<String>,
    pub checks: Option<Json<Vec<Check>>>,
    /// Given as `null`, nobody owns it.
    #[serde(default, deserialize_with = "given")]
    pub owner_id: Option<Option<String>>,
    pub docs: Option<Vec<String>>,
}

fn given<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Option<Option<String>>, D::Error> {
    Option::deserialize(from).map(Some)
}

/// A project's tasks, each status in its order.
pub async fn tasks(pool: &SqlitePool, team_id: &str, project_id: &str) -> sqlx::Result<Vec<Task>> {
    sqlx::query_as!(
        Task,
        r#"SELECT t.id, t.number, t.title, t.text, t.checks AS "checks: Json<Vec<Check>>", t.status,
                  t.owner_id, o.name AS "owner?", t.asker_id, a.name AS "asker?", t.created_at, t.chat_id,
                  (SELECT json_group_array(d.doc_id) FROM task_docs d WHERE d.task_id = t.id)
                      AS "docs!: Json<Vec<String>>"
           FROM projects p JOIN tasks t ON t.project_id = p.id
                LEFT JOIN users o ON o.id = t.owner_id LEFT JOIN users a ON a.id = t.asker_id
           WHERE p.id = ? AND p.team_id = ? ORDER BY t.status, t.position"#,
        project_id,
        team_id
    )
    .fetch_all(pool)
    .await
}

/// The new task's id, at the top of To do. `chat_id` is the chat whose agent makes it, for its owner.
pub async fn create_task(
    pool: &SqlitePool,
    project_id: &str,
    user_id: &str,
    chat_id: Option<&str>,
    fields: &TaskFields,
) -> sqlx::Result<String> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let id = new_id();
    let number = sqlx::query_scalar!(
        "UPDATE projects SET tasks_made = tasks_made + 1 WHERE id = ? RETURNING tasks_made",
        project_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let owner = fields.owner_id.as_ref().and_then(Option::as_ref);
    let text = fields.text.as_deref().unwrap_or_default();
    let none = Json(Vec::new());
    let checks = fields.checks.as_ref().unwrap_or(&none);
    sqlx::query!(
        "INSERT INTO tasks (id, project_id, number, title, text, checks, status, position, owner_id, asker_id, chat_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'todo',
                 (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE project_id = ?2 AND status = 'todo'),
                 ?7, ?8, ?9, unixepoch())",
        id,
        project_id,
        number,
        fields.title,
        text,
        checks,
        owner,
        user_id,
        chat_id
    )
    .execute(&mut *tx)
    .await?;
    if let Some(docs) = &fields.docs {
        set_task_docs(&mut tx, project_id, &id, docs).await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn change_task(
    pool: &SqlitePool,
    project_id: &str,
    task_id: &str,
    fields: &TaskFields,
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let (owned, owner) = (fields.owner_id.is_some(), fields.owner_id.as_ref());
    let owner = owner.and_then(Option::as_ref);
    sqlx::query!(
        "UPDATE tasks SET title = COALESCE(?, title), text = COALESCE(?, text), checks = COALESCE(?, checks),
                owner_id = CASE WHEN ? THEN ? ELSE owner_id END
         WHERE id = ? AND project_id = ?",
        fields.title,
        fields.text,
        fields.checks,
        owned,
        owner,
        task_id,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    if let Some(docs) = &fields.docs {
        set_task_docs(&mut tx, project_id, task_id, docs).await?;
    }
    tx.commit().await
}

/// Makes those of `docs` that are pages of the project the pages a task touches.
async fn set_task_docs(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    task_id: &str,
    docs: &[String],
) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM task_docs WHERE task_id = ?", task_id)
        .execute(&mut *tx)
        .await?;
    let docs = Json(docs);
    sqlx::query!(
        "INSERT INTO task_docs (task_id, doc_id)
         SELECT ?, id FROM docs WHERE project_id = ? AND id IN (SELECT value FROM json_each(?))",
        task_id,
        project_id,
        docs
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Puts the project's tasks named in `order` in `status`, in that order.
pub async fn order_tasks(
    pool: &SqlitePool,
    project_id: &str,
    status: &str,
    order: &[String],
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (position, id) in order.iter().enumerate() {
        let position = position as i64;
        sqlx::query!(
            "UPDATE tasks SET status = ?, position = ? WHERE id = ? AND project_id = ?",
            status,
            position,
            id,
            project_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn delete_task(pool: &SqlitePool, project_id: &str, task_id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM tasks WHERE id = ? AND project_id = ?",
        task_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A chat whose agent is using the server's tools.
pub struct ToolChat {
    pub id: String,
    pub team_id: String,
    pub project_id: String,
    /// The project's key.
    pub key: String,
    /// Whose chat it is, and so who its agent writes for.
    pub user_id: String,
    pub task_id: Option<String>,
}

/// The chat a running turn's token is of.
pub async fn tool_chat(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<ToolChat>> {
    sqlx::query_as!(
        ToolChat,
        "SELECT c.id, p.team_id, c.project_id, p.key, c.user_id, c.task_id
         FROM chats c JOIN projects p ON p.id = c.project_id
         WHERE c.tools_token = ? AND c.running = 1",
        token
    )
    .fetch_optional(pool)
    .await
}

/// Adds to a chat something the server did in it.
pub async fn note_chat(
    pool: &SqlitePool,
    chat_id: &str,
    body: &serde_json::Value,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    add_chat_event(&mut tx, chat_id, body).await?;
    tx.commit().await
}
