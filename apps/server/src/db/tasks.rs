use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::types::Json;

use super::new_id;

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
    pub owner_id: Option<String>,
    pub owner: Option<String>,
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
    /// A task goes to the end of a status it is new to. One being made is to do.
    pub status: Option<String>,
}

fn given<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Option<Option<String>>, D::Error> {
    Option::deserialize(from).map(Some)
}

/// A project's tasks, each status in its order.
pub async fn list(pool: &SqlitePool, team_id: &str, project_id: &str) -> sqlx::Result<Vec<Task>> {
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

/// The new task's id and number, at the top of To do. `chat_id` is the chat whose agent makes it, for its owner.
pub async fn create(
    pool: &SqlitePool,
    project_id: &str,
    user_id: &str,
    chat_id: Option<&str>,
    fields: &TaskFields,
) -> sqlx::Result<(String, i64)> {
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
        set_docs(&mut tx, project_id, &id, docs).await?;
    }
    tx.commit().await?;
    Ok((id, number))
}

pub async fn change(
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
        set_docs(&mut tx, project_id, task_id, docs).await?;
    }
    if let Some(status) = &fields.status {
        sqlx::query!(
            "UPDATE tasks SET status = ?1, position = (
                 SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE project_id = ?2 AND status = ?1)
             WHERE id = ?3 AND project_id = ?2 AND status != ?1",
            status,
            project_id,
            task_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Makes those of `docs` that are pages of the project the pages a task touches.
async fn set_docs(
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
pub async fn order(
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

pub async fn delete(pool: &SqlitePool, project_id: &str, task_id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM tasks WHERE id = ? AND project_id = ?",
        task_id,
        project_id
    )
    .execute(pool)
    .await?;
    Ok(())
}
