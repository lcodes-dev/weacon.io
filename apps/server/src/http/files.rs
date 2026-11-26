//! The files a project's docs show or link to: uploaded by its team, read by its team or,
//! for the pages behind a public link, by anyone who has the link.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ETAG, IF_NONE_MATCH, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::files::{File, Kept};
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::{AppState, db};

const MAX_IMAGE_BYTES: usize = 5_000_000;
pub const MAX_BYTES: usize = 20_000_000;

#[derive(Deserialize)]
pub struct Named {
    #[serde(default)]
    name: String,
}

/// Keeps the request's body as a file of the project, and answers with its id and whether it is an image.
pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(Named { name }): Query<Named>,
    bytes: Bytes,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    let image = image_kind(&bytes);
    if image.is_some() && bytes.len() > MAX_IMAGE_BYTES {
        return Err(ApiError::invalid("An image can be 5 MB at most"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(ApiError::invalid("A file can be 20 MB at most"));
    }
    // The name goes into a header when the file is downloaded, so it is kept plain.
    let plain = |c: &char| c.is_ascii_alphanumeric() || " .-_()".contains(*c);
    let name: String = name.chars().filter(plain).take(200).collect();
    let id = uuid::Uuid::now_v7().to_string();
    let path = state.files.join(&id);
    tokio::fs::write(&path, &bytes).await.map_err(|error| {
        tracing::error!("a file couldn't be kept: {error}");
        ApiError::internal("The file couldn't be kept")
    })?;
    let file = File {
        id: &id,
        kind: image.unwrap_or("application/octet-stream"),
        name: if name.trim().is_empty() {
            "file"
        } else {
            &name
        },
    };
    if !db::files::add(&state.pool, &team_id, &project_id, file, &user.id).await? {
        tokio::fs::remove_file(&path).await.ok();
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({"id": id, "image": image.is_some()})))
}

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, file_id)): Path<(String, String, String)>,
    asked: HeaderMap,
) -> Result<Response> {
    user.member_of(&team_id)?;
    let kept = db::files::find(&state.pool, &team_id, &project_id, &file_id).await?;
    served(&state, &file_id, kept, &asked).await
}

/// A file of a public link, when one of its pages shows or links to it.
pub async fn shared(
    State(state): State<AppState>,
    Path((token, file_id)): Path<(String, String)>,
    asked: HeaderMap,
) -> Result<Response> {
    let kept = db::files::shared(&state.pool, &token, &file_id).await?;
    served(&state, &file_id, kept, &asked).await
}

/// The file `id`, which the database said exists and the asker may have.
async fn served(
    state: &AppState,
    id: &str,
    kept: Option<Kept>,
    asked: &HeaderMap,
) -> Result<Response> {
    let Kept { kind, name } = kept.ok_or_else(ApiError::not_found)?;
    // A browser asks before it uses its copy, so that a file it may no longer have isn't shown
    // from there. A file never changes, so its id says whether the copy is good.
    let tag = format!("\"{id}\"");
    let caching = [
        (CACHE_CONTROL, "private, no-cache".to_owned()),
        (ETAG, tag.clone()),
    ];
    if asked
        .get(IF_NONE_MATCH)
        .is_some_and(|had| had.as_bytes() == tag.as_bytes())
    {
        return Ok((StatusCode::NOT_MODIFIED, caching).into_response());
    }
    let bytes = tokio::fs::read(state.files.join(id)).await;
    let bytes = bytes.map_err(|_| ApiError::not_found())?;
    // Only an image is shown. Anything else is downloaded, so that it can't run in the reader's browser.
    let disposition = if kind.starts_with("image/") {
        "inline".to_owned()
    } else {
        format!("attachment; filename=\"{name}\"")
    };
    let headers = [
        (CONTENT_TYPE, kind),
        (CONTENT_DISPOSITION, disposition),
        (X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
    ];
    Ok((caching, headers, bytes).into_response())
}

/// The image the bytes are, whatever the upload called itself. SVG is left out: it can carry scripts.
fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    }
}
