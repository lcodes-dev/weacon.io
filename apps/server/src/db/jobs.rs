use sqlx::SqlitePool;

use super::new_id;

pub struct Queued {
    pub id: String,
    pub job: String,
    pub started: bool,
}

/// The new job's id, or `None` when a job with its key is queued already.
pub async fn add(
    pool: &SqlitePool,
    job: &str,
    unique_key: Option<&str>,
) -> sqlx::Result<Option<String>> {
    let id = new_id();
    let added = sqlx::query!(
        "INSERT OR IGNORE INTO jobs (id, job, unique_key, created_at) VALUES (?, ?, ?, unixepoch())",
        id,
        job,
        unique_key
    )
    .execute(pool)
    .await?;
    Ok((added.rows_affected() > 0).then_some(id))
}

pub async fn start(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE jobs SET started_at = unixepoch() WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM jobs WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Oldest first.
pub async fn all(pool: &SqlitePool) -> sqlx::Result<Vec<Queued>> {
    sqlx::query_as!(
        Queued,
        r#"SELECT id, job, started_at IS NOT NULL AS "started!: bool" FROM jobs ORDER BY id"#
    )
    .fetch_all(pool)
    .await
}

pub async fn exists(pool: &SqlitePool, unique_key: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM jobs WHERE unique_key = ?) AS "found: bool""#,
        unique_key
    )
    .fetch_one(pool)
    .await
}
