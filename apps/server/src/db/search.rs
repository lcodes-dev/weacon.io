use sqlx::SqlitePool;

pub struct Unsearched {
    pub id: String,
    pub title: String,
    pub markdown: String,
    pub version: String,
}

pub async fn unsearched(
    pool: &SqlitePool,
    project_id: &str,
    model: &str,
) -> sqlx::Result<Vec<Unsearched>> {
    sqlx::query_as!(
        Unsearched,
        r#"SELECT id, title, markdown, version AS "version!" FROM (
               SELECT d.id, d.title, d.markdown, d.parts_version, d.parts_model, d.position,
                      (SELECT MAX(v.id) FROM doc_versions v WHERE v.doc_id = d.id) AS version
               FROM docs d WHERE d.project_id = ?)
           WHERE parts_version IS NOT version OR parts_model IS NOT ? ORDER BY position"#,
        project_id,
        model
    )
    .fetch_all(pool)
    .await
}

pub async fn set_parts(
    pool: &SqlitePool,
    doc_id: &str,
    version: &str,
    model: &str,
    parts: &[(String, Vec<u8>)],
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query!("DELETE FROM doc_parts WHERE doc_id = ?", doc_id)
        .execute(&mut *tx)
        .await?;
    for (position, (text, embedding)) in parts.iter().enumerate() {
        let position = position as i64;
        sqlx::query!(
            "INSERT INTO doc_parts (doc_id, position, text, embedding) VALUES (?, ?, ?, ?)",
            doc_id,
            position,
            text,
            embedding
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE docs SET parts_version = ?, parts_model = ? WHERE id = ?",
        version,
        model,
        doc_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

pub struct DocPart {
    pub doc_id: String,
    pub title: String,
    pub files: String,
    pub text: String,
    pub embedding: Vec<u8>,
}

pub async fn parts(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<DocPart>> {
    sqlx::query_as!(
        DocPart,
        "SELECT p.doc_id, d.title, d.files, p.text, p.embedding
         FROM docs d JOIN doc_parts p ON p.doc_id = d.id
         WHERE d.project_id = ? ORDER BY d.position, p.position",
        project_id
    )
    .fetch_all(pool)
    .await
}
