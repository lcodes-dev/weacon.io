//! Editing a project's docs by hand: its pages, their versions and their files.

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

use crate::db::{self, DocEdit, DocVersion, File, Kept, User};
use crate::teams::{membership, refused};
use crate::{ApiError, AppState, docs, not_found};

const IMAGE_BYTES: usize = 5_000_000;
pub const FILE_BYTES: usize = 20_000_000;

fn titled(title: &str) -> Result<&str, ApiError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(refused("Give the page a title"));
    }
    Ok(title)
}

#[derive(Deserialize)]
pub struct NewPage {
    section: String,
    title: String,
}

pub async fn add(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(page): Json<NewPage>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    if !["works", "built"].contains(&page.section.as_str()) {
        return Err(refused(
            "A page is added to the user docs or the technical docs",
        ));
    }
    let title = titled(&page.title)?;
    let added = db::add_doc(
        &state.pool,
        &team_id,
        &project_id,
        &page.section,
        title,
        &user.id,
    );
    let id = added.await?.ok_or_else(not_found)?;
    Ok(Json(json!({"id": id})))
}

pub async fn save(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): Path<(String, String, String)>,
    Json(edit): Json<DocEdit>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    titled(&edit.title)?;
    let saved = db::save_doc(
        &state.pool,
        &team_id,
        &project_id,
        &page_id,
        &edit,
        &user.id,
    );
    if !saved.await?.ok_or_else(not_found)? {
        let message = "Someone else changed this page meanwhile";
        return Err(ApiError(StatusCode::CONFLICT, message.into()));
    }
    // The save doesn't wait for the writer's check of it.
    if let Some(writer) = db::writer(&state.pool, &team_id).await? {
        db::start_check(&state.pool, &page_id).await?;
        tokio::spawn(docs::check(state, writer, team_id, project_id, page_id));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Accepted {
    /// The version of the proposal's page that the person looked at; none for a new page.
    version: Option<String>,
    /// The text they made of the proposal's, when they took only some of its changes.
    markdown: Option<String>,
}

/// Does what a proposal suggests.
pub async fn accept(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, proposal_id)): Path<(String, String, String)>,
    Json(accepted): Json<Accepted>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    let accepted = db::accept_proposal(
        &state.pool,
        &team_id,
        &project_id,
        &proposal_id,
        accepted.version.as_deref(),
        accepted.markdown.as_deref(),
        &user.id,
    );
    if !accepted.await?.ok_or_else(not_found)? {
        let message = "The page changed after this proposal was shown. Look at it again";
        return Err(ApiError(StatusCode::CONFLICT, message.into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reject(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, proposal_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    if !db::delete_proposal(&state.pool, &team_id, &project_id, &proposal_id).await? {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Takes one of the writer's notes off a page.
pub async fn dismiss(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id, index)): Path<(String, String, String, i64)>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    if !db::dismiss_note(&state.pool, &team_id, &project_id, &page_id, index).await? {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    if !db::delete_doc(&state.pool, &team_id, &project_id, &page_id).await? {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Puts the pages in the order of the ids given.
pub async fn order(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(ids): Json<Vec<String>>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    db::order_docs(&state.pool, &team_id, &project_id, &ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn versions(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): Path<(String, String, String)>,
) -> Result<Json<Vec<DocVersion>>, ApiError> {
    membership(&user, &team_id)?;
    let versions = db::doc_versions(&state.pool, &team_id, &project_id, &page_id).await?;
    if versions.is_empty() {
        return Err(not_found());
    }
    Ok(Json(versions))
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

#[derive(Deserialize)]
pub struct Named {
    #[serde(default)]
    name: String,
}

/// Keeps the request's body as a file of the project, and answers with its id and whether it is an image.
pub async fn upload(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(Named { name }): Query<Named>,
    bytes: Bytes,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let image = image_kind(&bytes);
    if image.is_some() && bytes.len() > IMAGE_BYTES {
        return Err(refused("An image can be 5 MB at most"));
    }
    if bytes.len() > FILE_BYTES {
        return Err(refused("A file can be 20 MB at most"));
    }
    // The name goes into a header when the file is downloaded, so it is kept plain.
    let plain = |c: &char| c.is_ascii_alphanumeric() || " .-_()".contains(*c);
    let name: String = name.chars().filter(plain).take(200).collect();
    let id = uuid::Uuid::now_v7().to_string();
    let path = state.files.join(&id);
    tokio::fs::write(&path, &bytes).await.map_err(unkept)?;
    let file = File {
        id: &id,
        kind: image.unwrap_or("application/octet-stream"),
        name: if name.trim().is_empty() {
            "file"
        } else {
            &name
        },
    };
    if !db::add_file(&state.pool, &team_id, &project_id, file, &user.id).await? {
        tokio::fs::remove_file(&path).await.ok();
        return Err(not_found());
    }
    Ok(Json(json!({"id": id, "image": image.is_some()})))
}

fn unkept(error: std::io::Error) -> ApiError {
    tracing::error!("a file couldn't be kept: {error}");
    let message = "The file couldn't be kept";
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, message.into())
}

/// The file `id`, which the database said exists and the asker may have.
async fn served(
    state: &AppState,
    id: &str,
    kept: Option<Kept>,
    asked: &HeaderMap,
) -> Result<Response, ApiError> {
    let Kept { kind, name } = kept.ok_or_else(not_found)?;
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
    // Only an image is shown. Anything else is downloaded, so that it can't run in the reader's browser.
    let shown = kind.starts_with("image/");
    let disposition = format!("attachment; filename=\"{name}\"");
    let headers = [
        (CONTENT_TYPE, kind),
        (
            CONTENT_DISPOSITION,
            if shown { "inline".into() } else { disposition },
        ),
        (X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
    ];
    Ok((caching, headers, bytes.map_err(|_| not_found())?).into_response())
}

pub async fn file(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, file_id)): Path<(String, String, String)>,
    asked: HeaderMap,
) -> Result<Response, ApiError> {
    membership(&user, &team_id)?;
    let kept = db::file(&state.pool, &team_id, &project_id, &file_id).await?;
    served(&state, &file_id, kept, &asked).await
}

/// A file of a public link, to anyone who has the link, when one of its pages shows or links to it.
pub async fn shared_file(
    State(state): State<AppState>,
    Path((token, file_id)): Path<(String, String)>,
    asked: HeaderMap,
) -> Result<Response, ApiError> {
    let kept = db::shared_file(&state.pool, &token, &file_id).await?;
    served(&state, &file_id, kept, &asked).await
}
