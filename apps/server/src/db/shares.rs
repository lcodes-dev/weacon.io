use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Serialize)]
pub struct Share {
    pub token: String,
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

pub async fn share(
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

pub async fn list(pool: &SqlitePool, team_id: &str, project_id: &str) -> sqlx::Result<Vec<Share>> {
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

pub async fn end(
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

pub async fn find(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<Shared>> {
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
