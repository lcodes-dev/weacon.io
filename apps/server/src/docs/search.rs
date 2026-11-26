use doc_search::MODEL;
use sqlx::SqlitePool;

use crate::AppState;
use crate::error::{ApiError, Result};

pub type Embed = fn(&[String]) -> Result<Vec<Vec<f32>>, String>;

async fn embedded(state: &AppState, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let embed = state.embed;
    let done = tokio::task::spawn_blocking(move || embed(&texts)).await;
    let done = done.unwrap_or_else(|error| Err(error.to_string()));
    done.map_err(ApiError::bad_gateway)
}

pub async fn search(state: &AppState, project_id: &str, question: &str) -> Result<Vec<DocPart>> {
    for page in unsearched(&state.pool, project_id, MODEL).await? {
        let texts = doc_search::parts(&page.title, &page.markdown);
        let embeddings = embedded(state, texts.clone()).await?;
        let bytes =
            |embedding: Vec<f32>| embedding.into_iter().flat_map(f32::to_le_bytes).collect();
        let parts: Vec<(String, Vec<u8>)> = texts
            .into_iter()
            .zip(embeddings.into_iter().map(bytes))
            .collect();
        set_parts(&state.pool, &page.id, &page.version, MODEL, &parts).await?;
    }
    let parts = parts(&state.pool, project_id).await?;
    if parts.is_empty() {
        return Ok(parts);
    }
    let asked = embedded(state, vec![question.to_owned()]).await?.remove(0);
    let score = |part: DocPart| {
        let (fours, _) = part.embedding.as_chunks::<4>();
        let embedding: Vec<f32> = fours.iter().map(|four| f32::from_le_bytes(*four)).collect();
        (
            doc_search::score(question, &asked, &part.text, &embedding),
            part,
        )
    };
    let mut scored: Vec<(f32, DocPart)> = parts.into_iter().map(score).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(scored.into_iter().map(|(_, part)| part).collect())
}

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
