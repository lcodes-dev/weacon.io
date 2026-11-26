use sqlx::SqlitePool;

/// A file kept for a project's docs.
pub struct File<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
}

/// False when the team has no such project.
pub async fn add(
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
pub async fn find(
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
pub async fn shared(pool: &SqlitePool, token: &str, id: &str) -> sqlx::Result<Option<Kept>> {
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
