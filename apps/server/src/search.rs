use doc_search::MODEL;

use crate::db::search::DocPart;
use crate::error::{ApiError, Result};
use crate::{AppState, db};

pub type Embed = fn(&[String]) -> Result<Vec<Vec<f32>>, String>;

async fn embedded(state: &AppState, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let embed = state.embed;
    let done = tokio::task::spawn_blocking(move || embed(&texts)).await;
    let done = done.unwrap_or_else(|error| Err(error.to_string()));
    done.map_err(ApiError::bad_gateway)
}

pub async fn search(state: &AppState, project_id: &str, question: &str) -> Result<Vec<DocPart>> {
    for page in db::search::unsearched(&state.pool, project_id, MODEL).await? {
        let texts = doc_search::parts(&page.title, &page.markdown);
        let embeddings = embedded(state, texts.clone()).await?;
        let bytes =
            |embedding: Vec<f32>| embedding.into_iter().flat_map(f32::to_le_bytes).collect();
        let parts: Vec<(String, Vec<u8>)> = texts
            .into_iter()
            .zip(embeddings.into_iter().map(bytes))
            .collect();
        db::search::set_parts(&state.pool, &page.id, &page.version, MODEL, &parts).await?;
    }
    let parts = db::search::parts(&state.pool, project_id).await?;
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
