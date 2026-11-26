use serde::Serialize;
use sqlx::SqlitePool;

use super::new_id;

#[derive(Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub key: String,
    pub repo: Option<String>,
    pub branches: Vec<Branch>,
}

#[derive(Serialize)]
pub struct Branch {
    #[serde(skip)]
    pub project_id: String,
    pub name: String,
    pub head_sha: Option<String>,
    pub commit_sha: Option<String>,
    pub error: Option<String>,
}

pub struct Tracked {
    pub id: String,
    pub team_id: String,
    pub repo: String,
}

pub struct Waiting {
    pub project_id: String,
    pub team_id: String,
    pub repo: String,
    pub name: String,
    pub head_sha: String,
}

pub async fn list(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Vec<Project>> {
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

pub async fn create(
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

pub async fn update(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    name: &str,
    key: &str,
    repo: Option<&str>,
    branches: &[String],
) -> sqlx::Result<Option<bool>> {
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
    super::maps::forget_unlisted_blobs(tx, project_id).await?;
    Ok(changed)
}

pub async fn branch_names(
    db: impl sqlx::SqliteExecutor<'_>,
    project_id: &str,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!("SELECT name FROM branches WHERE project_id = ?", project_id)
        .fetch_all(db)
        .await
}

pub async fn tracked(pool: &SqlitePool) -> sqlx::Result<Vec<Tracked>> {
    sqlx::query_as!(
        Tracked,
        r#"SELECT id, team_id, repo AS "repo!" FROM projects
           WHERE repo IS NOT NULL AND id IN (SELECT project_id FROM branches)"#
    )
    .fetch_all(pool)
    .await
}

pub async fn set_branch_head(
    pool: &SqlitePool,
    project_id: &str,
    name: &str,
    head_sha: Option<&str>,
    error: Option<&str>,
) -> sqlx::Result<bool> {
    let changed = sqlx::query!(
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
    Ok(changed.rows_affected() > 0)
}

pub async fn waiting(pool: &SqlitePool) -> sqlx::Result<Vec<Waiting>> {
    sqlx::query_as!(
        Waiting,
        r#"SELECT b.project_id, p.team_id, p.repo AS "repo!", b.name, b.head_sha AS "head_sha!"
           FROM branches b JOIN projects p ON p.id = b.project_id
           WHERE b.head_sha IS NOT NULL AND b.head_sha IS NOT b.commit_sha AND b.error IS NULL"#
    )
    .fetch_all(pool)
    .await
}

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

pub async fn delete(
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

pub async fn people(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT m.user_id FROM projects p JOIN team_members m ON m.team_id = p.team_id
         WHERE p.id = ?",
        project_id
    )
    .fetch_all(pool)
    .await
}
