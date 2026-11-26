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

use super::db::{self, DocEdit, DocVersion, File, Kept, Share, Shared};
use super::jobs::{AnswerDocsRequest, CheckPage, Request, WriteDocs};
use crate::accounts::db::User;
use crate::auth::random_token;
use crate::error::{ApiError, Result};
use crate::jobs::Job;
use crate::live::Topic;
use crate::{AppState, jobs, live, now, projects, settings};

pub async fn show_docs(
    State(state): State<AppState>,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let status = db::status(&state.pool, &team_id, &project_id).await?.ok_or_else(ApiError::not_found)?;
    let pages = db::list(&state.pool, &project_id).await?;
    let proposals = db::list_proposals(&state.pool, &project_id).await?;
    let left_out = status.left_out.as_deref().unwrap_or("[]");
    let left_out: Value = serde_json::from_str(left_out).unwrap_or_default();
    Ok(Json(json!({
        "state": status.state,
        "message": status.message,
        "branch": status.branch,
        "commit": status.commit,
        "left_out": left_out,
        "pages": pages,
        "proposals": proposals,
    })))
}

#[derive(Deserialize)]
pub struct FromBranch {
    branch: String,
}

pub async fn write_docs(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(FromBranch { branch }): Json<FromBranch>,
) -> Result<StatusCode> {
    let run = start_run(&state, &user, &team_id, &project_id, &branch).await?;
    let job = WriteDocs { team_id, project_id, run, branch, user_id: user.id };
    jobs::perform_later(&state, Job::WriteDocs(job)).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn ask_docs(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(request): Json<Request>,
) -> Result<StatusCode> {
    if request.text.trim().is_empty() {
        return Err(ApiError::invalid("Say what to change"));
    }
    let run = start_run(&state, &user, &team_id, &project_id, &request.branch).await?;
    let job = AnswerDocsRequest { team_id, project_id, run, user_id: user.id, request };
    jobs::perform_later(&state, Job::AnswerDocsRequest(job)).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn start_run(
    state: &AppState,
    user: &User,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> Result<i64> {
    let commit = projects::db::branch_commit(&state.pool, team_id, project_id, branch).await?;
    if commit.ok_or_else(ApiError::not_found)?.is_none() {
        return Err(ApiError::invalid("This branch isn't mapped yet"));
    }
    super::writer(state, team_id, &user.id).await?;
    let run = rand::random::<u32>().into();
    if !db::claim_run(&state.pool, project_id, run).await? {
        return Err(ApiError::invalid("The docs are already being written"));
    }
    live::project(state, project_id, Topic::Docs).await;
    Ok(run)
}

type PagePath = Path<(String, String, String)>;

#[derive(Deserialize)]
pub struct NewPage {
    section: String,
    title: String,
}

pub async fn create_page(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(page): Json<NewPage>,
) -> Result<Json<Value>> {
    if !["works", "built"].contains(&page.section.as_str()) {
        let message = "A page is added to the user docs or the technical docs";
        return Err(ApiError::invalid(message));
    }
    let title = titled(&page.title)?;
    let added = db::add(&state.pool, &team_id, &project_id, &page.section, title, &user.id).await?;
    let id = added.ok_or_else(ApiError::not_found)?;
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(Json(json!({"id": id})))
}

pub async fn update_page(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): PagePath,
    Json(edit): Json<DocEdit>,
) -> Result<StatusCode> {
    titled(&edit.title)?;
    let saved = db::save(&state.pool, &team_id, &project_id, &page_id, &edit, &user.id);
    if !saved.await?.ok_or_else(ApiError::not_found)? {
        return Err(ApiError::conflict("Someone else changed this page meanwhile"));
    }
    if settings::db::writer(&state.pool, &state.server_key, &team_id).await?.is_some() {
        db::start_check(&state.pool, &page_id).await?;
        let job = CheckPage { team_id: team_id.clone(), project_id, doc_id: page_id, user_id: user.id };
        jobs::perform_later(&state, Job::CheckPage(job)).await?;
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_page(
    State(state): State<AppState>,
    Path((team_id, project_id, page_id)): PagePath,
) -> Result<StatusCode> {
    if !db::delete(&state.pool, &team_id, &project_id, &page_id).await? {
        return Err(ApiError::not_found());
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn order_pages(
    State(state): State<AppState>,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(ids): Json<Vec<String>>,
) -> Result<StatusCode> {
    db::order(&state.pool, &team_id, &project_id, &ids).await?;
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn page_versions(
    State(state): State<AppState>,
    Path((team_id, project_id, page_id)): PagePath,
) -> Result<Json<Vec<DocVersion>>> {
    let versions = db::versions(&state.pool, &team_id, &project_id, &page_id).await?;
    if versions.is_empty() {
        return Err(ApiError::not_found());
    }
    Ok(Json(versions))
}

pub async fn dismiss_note(
    State(state): State<AppState>,
    Path((team_id, project_id, page_id, index)): Path<(String, String, String, i64)>,
) -> Result<StatusCode> {
    if !db::dismiss_note(&state.pool, &team_id, &project_id, &page_id, index).await? {
        return Err(ApiError::not_found());
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

fn titled(title: &str) -> Result<&str> {
    let title = title.trim();
    if title.is_empty() {
        return Err(ApiError::invalid("Give the page a title"));
    }
    Ok(title)
}

#[derive(Deserialize)]
pub struct Accepted {
    version: Option<String>,
    markdown: Option<String>,
}

pub async fn accept_proposal(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, proposal_id)): Path<(String, String, String)>,
    Json(accepted): Json<Accepted>,
) -> Result<StatusCode> {
    let accepted = db::accept_proposal(
        &state.pool,
        &team_id,
        &project_id,
        &proposal_id,
        accepted.version.as_deref(),
        accepted.markdown.as_deref(),
        &user.id,
    );
    if !accepted.await?.ok_or_else(ApiError::not_found)? {
        let message = "The page changed after this proposal was shown. Look at it again";
        return Err(ApiError::conflict(message));
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_proposal(
    State(state): State<AppState>,
    Path((team_id, project_id, proposal_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    if !db::delete_proposal(&state.pool, &team_id, &project_id, &proposal_id).await? {
        return Err(ApiError::not_found());
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

const MAX_IMAGE_BYTES: usize = 5_000_000;
pub const MAX_BYTES: usize = 20_000_000;

#[derive(Deserialize)]
pub struct Named {
    #[serde(default)]
    name: String,
}

pub async fn upload_file(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(Named { name }): Query<Named>,
    bytes: Bytes,
) -> Result<Json<Value>> {
    let image = image_kind(&bytes);
    if image.is_some() && bytes.len() > MAX_IMAGE_BYTES {
        return Err(ApiError::invalid("An image can be 5 MB at most"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(ApiError::invalid("A file can be 20 MB at most"));
    }
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
        name: if name.trim().is_empty() { "file" } else { &name },
    };
    if !db::add_file(&state.pool, &team_id, &project_id, file, &user.id).await? {
        tokio::fs::remove_file(&path).await.ok();
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({"id": id, "image": image.is_some()})))
}

pub async fn show_file(
    State(state): State<AppState>,
    Path((team_id, project_id, file_id)): Path<(String, String, String)>,
    asked: HeaderMap,
) -> Result<Response> {
    let kept = db::find_file(&state.pool, &team_id, &project_id, &file_id).await?;
    served(&state, &file_id, kept, &asked).await
}

pub async fn shared_file(
    State(state): State<AppState>,
    Path((token, file_id)): Path<(String, String)>,
    asked: HeaderMap,
) -> Result<Response> {
    let kept = db::shared_file(&state.pool, &token, &file_id).await?;
    served(&state, &file_id, kept, &asked).await
}

async fn served(state: &AppState, id: &str, kept: Option<Kept>, asked: &HeaderMap) -> Result<Response> {
    let Kept { kind, name } = kept.ok_or_else(ApiError::not_found)?;
    let tag = format!("\"{id}\"");
    let caching = [(CACHE_CONTROL, "private, no-cache".to_owned()), (ETAG, tag.clone())];
    if asked.get(IF_NONE_MATCH).is_some_and(|had| had.as_bytes() == tag.as_bytes()) {
        return Ok((StatusCode::NOT_MODIFIED, caching).into_response());
    }
    let bytes = tokio::fs::read(state.files.join(id)).await.map_err(|_| ApiError::not_found())?;
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

fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

pub async fn list_shares(
    State(state): State<AppState>,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Vec<Share>>> {
    Ok(Json(db::list_shares(&state.pool, &team_id, &project_id).await?))
}

pub async fn create_share(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let pages = db::list(&state.pool, &project_id).await?;
    let pages: Vec<(&str, &str)> = pages
        .iter()
        .filter(|page| page.section == "works")
        .map(|page| (page.title.as_str(), page.markdown.as_str()))
        .collect();
    if pages.is_empty() {
        return Err(ApiError::invalid("This project has no user docs to share"));
    }
    let token = random_token();
    let shared = db::share(&state.pool, &team_id, &project_id, &token, &user.id, now(), &pages);
    if !shared.await? {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({"token": token})))
}

pub async fn delete_share(
    State(state): State<AppState>,
    Path((team_id, project_id, token)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    if !db::end_share(&state.pool, &team_id, &project_id, &token).await? {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn show_share(State(state): State<AppState>, Path(token): Path<String>) -> Result<Json<Shared>> {
    let shared = db::find_share(&state.pool, &token).await?;
    Ok(Json(shared.ok_or_else(ApiError::not_found)?))
}
