use std::collections::{HashMap, HashSet};

use code_map::{Reading, Symbol};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::db::new_id;

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

pub async fn exists(pool: &SqlitePool, team_id: &str, project_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM projects WHERE id = ? AND team_id = ?) AS "found: bool""#,
        project_id,
        team_id
    )
    .fetch_one(pool)
    .await
}

pub async fn list(pool: &SqlitePool, team_id: &str) -> sqlx::Result<Vec<Project>> {
    let projects =
        sqlx::query!("SELECT id, name, key, repo FROM projects WHERE team_id = ? ORDER BY name", team_id)
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
        branches: branches.extract_if(.., |branch| branch.project_id == project.id).collect(),
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
    let old_repo =
        sqlx::query_scalar!("SELECT repo FROM projects WHERE team_id = ? AND id = ?", team_id, project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(old_repo) = old_repo else {
        return Ok(None);
    };
    sqlx::query!("UPDATE projects SET name = ?, key = ?, repo = ? WHERE id = ?", name, key, repo, project_id)
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
        sqlx::query!("DELETE FROM proposals WHERE project_id = ?", project_id).execute(&mut *tx).await?;
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
            sqlx::query!("DELETE FROM branches WHERE project_id = ? AND name = ?", project_id, name)
                .execute(&mut *tx)
                .await?;
            changed = true;
        }
    }
    for name in names {
        let added =
            sqlx::query!("INSERT OR IGNORE INTO branches (project_id, name) VALUES (?, ?)", project_id, name)
                .execute(&mut *tx)
                .await?;
        changed |= added.rows_affected() > 0;
    }
    forget_unlisted_blobs(tx, project_id).await?;
    Ok(changed)
}

pub async fn branch_names(db: impl sqlx::SqliteExecutor<'_>, project_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!("SELECT name FROM branches WHERE project_id = ?", project_id).fetch_all(db).await
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

pub async fn delete(pool: &SqlitePool, team_id: &str, project_id: &str) -> sqlx::Result<Vec<String>> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let files = sqlx::query_scalar!(
        "SELECT f.id FROM files f JOIN projects p ON p.id = f.project_id
         WHERE p.id = ? AND p.team_id = ?",
        project_id,
        team_id
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM projects WHERE team_id = ? AND id = ?", team_id, project_id)
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

#[derive(Serialize)]
pub struct Map {
    pub commit: Option<String>,
    pub files: Vec<MapFile>,
    pub uses: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct MapFile {
    pub path: String,
    pub language: Option<&'static str>,
    pub lines: i64,
    pub symbols: Vec<Symbol>,
}

pub(super) async fn forget_unlisted_blobs(
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

pub async fn forget_other_readers(pool: &SqlitePool, reader: i64) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar!("SELECT map_reader FROM config").fetch_one(&mut *tx).await?;
    if current != reader {
        sqlx::query!("DELETE FROM branch_files").execute(&mut *tx).await?;
        sqlx::query!("DELETE FROM blobs").execute(&mut *tx).await?;
        sqlx::query!("UPDATE branches SET commit_sha = NULL").execute(&mut *tx).await?;
        sqlx::query!("UPDATE config SET map_reader = ?", reader).execute(&mut *tx).await?;
    }
    tx.commit().await
}

pub async fn known_blobs(pool: &SqlitePool, project_id: &str) -> sqlx::Result<HashSet<String>> {
    let blobs = sqlx::query_scalar!("SELECT blob FROM blobs WHERE project_id = ?", project_id)
        .fetch_all(pool)
        .await?;
    Ok(blobs.into_iter().collect())
}

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
    sqlx::query!("DELETE FROM branch_files WHERE project_id = ? AND branch = ?", project, branch.name)
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

pub async fn find_map(
    pool: &SqlitePool,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<Map>> {
    let mut tx = pool.begin().await?;
    let Some((commit, files)) = read_files(&mut tx, team_id, project_id, branch).await? else {
        return Ok(None);
    };
    let uses = uses(&mut tx, project_id, branch, &files).await?;
    Ok(Some(Map { commit, files, uses }))
}

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
        let symbol = Symbol { name: row.name, kind: row.kind, line: row.line, end_line: row.end_line };
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
