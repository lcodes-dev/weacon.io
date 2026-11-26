use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::{as_json, as_optional_json, new_id};

pub struct DocsStatus {
    pub state: String,
    pub message: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub left_out: Option<String>,
}

#[derive(Serialize)]
pub struct Doc {
    pub id: String,
    pub section: String,
    pub title: String,
    pub markdown: String,
    pub version: String,
    #[serde(serialize_with = "as_json")]
    pub files: String,
    #[serde(serialize_with = "as_json")]
    pub unread: String,
    pub commit: Option<String>,
    #[serde(skip)]
    pub written: Option<String>,
    #[serde(serialize_with = "as_optional_json")]
    pub notes: Option<String>,
    pub checking: bool,
}

pub struct Written {
    pub doc: Option<(String, String)>,
    pub section: String,
    pub title: String,
    pub markdown: Option<String>,
    pub files: String,
    pub unread: String,
    pub notes: String,
    pub reason: String,
}

impl Written {
    pub(super) fn text<'a>(&'a self, markdown: &'a str, commit: Option<&'a str>) -> Text<'a> {
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

pub(super) struct Text<'a> {
    pub(super) section: &'a str,
    pub(super) title: &'a str,
    pub(super) markdown: &'a str,
    pub(super) files: &'a str,
    pub(super) unread: &'a str,
    pub(super) commit: Option<&'a str>,
}

#[derive(Serialize)]
pub struct DocVersion {
    pub id: String,
    pub title: String,
    pub markdown: String,
    pub author: Option<String>,
    pub created_at: i64,
}

pub async fn status(
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

pub async fn list(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Doc>> {
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

pub async fn claim_run(pool: &SqlitePool, project_id: &str, run: i64) -> sqlx::Result<bool> {
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

pub async fn set_state(
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
        sqlx::query!(
            "DELETE FROM proposals WHERE project_id = ? AND agent IS NULL",
            project_id
        )
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

pub async fn start_check(pool: &SqlitePool, doc_id: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE docs SET checking = 1 WHERE id = ?", doc_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn stop_check(pool: &SqlitePool, doc_id: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE docs SET checking = 0 WHERE id = ?", doc_id)
        .execute(pool)
        .await?;
    Ok(())
}

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

pub(super) async fn add_page(
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

pub(super) async fn add_version(
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

pub async fn add(
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
    pub version: String,
}

pub async fn save(
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

pub async fn delete(
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

pub async fn order(
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

pub async fn versions(
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
