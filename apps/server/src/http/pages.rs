//! A project's docs pages, as people edit them by hand.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::docs::{DocEdit, DocVersion};
use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::jobs::{self, CheckPage, Job};
use crate::live::{self, Topic};
use crate::{AppState, db};

type PagePath = Path<(String, String, String)>;

#[derive(Deserialize)]
pub struct NewPage {
    section: String,
    title: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(page): Json<NewPage>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    if !["works", "built"].contains(&page.section.as_str()) {
        let message = "A page is added to the user docs or the technical docs";
        return Err(ApiError::invalid(message));
    }
    let title = titled(&page.title)?;
    let added = db::docs::add(
        &state.pool,
        &team_id,
        &project_id,
        &page.section,
        title,
        &user.id,
    )
    .await?;
    let id = added.ok_or_else(ApiError::not_found)?;
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(Json(json!({"id": id})))
}

/// Saves a person's edit, and has the team's writer, if it has one, check it against the code.
pub async fn update(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): PagePath,
    Json(edit): Json<DocEdit>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    titled(&edit.title)?;
    let saved = db::docs::save(
        &state.pool,
        &team_id,
        &project_id,
        &page_id,
        &edit,
        &user.id,
    );
    if !saved.await?.ok_or_else(ApiError::not_found)? {
        return Err(ApiError::conflict(
            "Someone else changed this page meanwhile",
        ));
    }
    if db::settings::writer(&state.pool, &state.server_key, &team_id)
        .await?
        .is_some()
    {
        db::docs::start_check(&state.pool, &page_id).await?;
        let job = CheckPage {
            team_id: team_id.clone(),
            project_id,
            doc_id: page_id,
            user_id: user.id,
        };
        jobs::dispatch(&state, Job::CheckPage(job)).await?;
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): PagePath,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    if !db::docs::delete(&state.pool, &team_id, &project_id, &page_id).await? {
        return Err(ApiError::not_found());
    }
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Puts the pages in the order of the ids given.
pub async fn order(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(ids): Json<Vec<String>>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    db::docs::order(&state.pool, &team_id, &project_id, &ids).await?;
    live::team(&state, &team_id, Topic::Docs).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn versions(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id)): PagePath,
) -> Result<Json<Vec<DocVersion>>> {
    user.member_of(&team_id)?;
    let versions = db::docs::versions(&state.pool, &team_id, &project_id, &page_id).await?;
    if versions.is_empty() {
        return Err(ApiError::not_found());
    }
    Ok(Json(versions))
}

/// Takes one of the writer's notes off a page.
pub async fn dismiss_note(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, page_id, index)): Path<(String, String, String, i64)>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    if !db::docs::dismiss_note(&state.pool, &team_id, &project_id, &page_id, index).await? {
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
