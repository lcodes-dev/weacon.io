use serde::Serialize;
use sqlx::SqlitePool;

use super::{as_json, as_optional_json, new_id};

/// A change the writer or an agent suggests.
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
    #[serde(serialize_with = "as_optional_json")]
    pub notes: Option<String>,
    /// A JSON list of the files the writer had no room to read.
    #[serde(serialize_with = "as_json")]
    pub unread: String,
    pub reason: String,
    pub request: Option<String>,
    /// For an agent's: which agent, whom it works for, and its chat if the team may read it.
    pub agent: Option<String>,
    pub user: Option<String>,
    pub chat_id: Option<String>,
}

pub async fn list(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Proposal>> {
    sqlx::query_as!(
        Proposal,
        r#"SELECT p.id, p.doc_id AS page, p.base, p.section, p.title, p.markdown,
                  p.notes AS "notes?", p.unread, p.reason, p.request, p.agent, u.name AS "user?",
                  c.id AS "chat_id?"
           FROM proposals p
           LEFT JOIN users u ON u.id = p.user_id
           LEFT JOIN chats c ON c.id = p.chat_id AND c.group_id IS NULL
           WHERE p.project_id = ? ORDER BY p.id"#,
        project_id
    )
    .fetch_all(pool)
    .await
}

/// Keeps what an agent proposes for the docs of its project: a new text for the page `doc`, or
/// a new page in `section`.
pub async fn propose(
    pool: &SqlitePool,
    caller: &super::chats::Caller,
    doc: Option<&super::docs::Doc>,
    section: &str,
    title: &str,
    markdown: &str,
    reason: &str,
) -> sqlx::Result<()> {
    let id = new_id();
    let (doc_id, base) = doc.map(|doc| (&doc.id, &doc.version)).unzip();
    // A changed page keeps what it was written from; a new one has nothing yet.
    let files = doc.map_or("[]", |doc| &doc.files);
    let unread = doc.map_or("[]", |doc| &doc.unread);
    let commit = doc.and_then(|doc| doc.commit.as_ref());
    sqlx::query!(
        "INSERT INTO proposals (id, project_id, doc_id, base, section, title, markdown, files,
                                unread, commit_sha, notes, reason, agent, user_id, chat_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '[]', ?, ?, ?, ?, unixepoch())",
        id,
        caller.project_id,
        doc_id,
        base,
        section,
        title,
        markdown,
        files,
        unread,
        commit,
        reason,
        caller.agent,
        caller.user_id,
        caller.chat_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Does what a proposal suggests and deletes it. `seen` is the version of its page that the
/// person accepting it looked at. `markdown` is the text they made of the proposal's, saved
/// under their name; without it the proposal's own is saved as the writer's.
/// `None` when the team's project has no such proposal, false when the page has changed
/// since `seen`, which leaves the proposal as it was.
pub async fn accept(
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
    let text = |markdown| super::docs::Text {
        section: &proposal.section,
        title: &proposal.title,
        markdown,
        files: &proposal.files,
        unread: &proposal.unread,
        commit: proposal.commit_sha.as_deref(),
    };
    match (&proposal.doc_id, markdown.or(proposal.markdown.as_deref())) {
        (None, markdown) => {
            super::docs::add_page(
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
            super::docs::add_version(&mut tx, doc_id, text.title, text.markdown, author).await?;
        }
    }
    tx.commit().await?;
    Ok(Some(true))
}

pub async fn delete(
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
