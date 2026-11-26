use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::users::User;
use crate::error::{ApiError, Result};
use crate::jobs::answer_docs_request::Request;
use crate::jobs::{self, AnswerDocsRequest, Job, WriteDocs};
use crate::live::{self, Topic};
use crate::{AppState, db, docs};

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    user.member_of(&team_id)?;
    let status = db::docs::status(&state.pool, &team_id, &project_id).await?;
    let status = status.ok_or_else(ApiError::not_found)?;
    let pages = db::docs::list(&state.pool, &project_id).await?;
    let proposals = db::proposals::list(&state.pool, &project_id).await?;
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

/// Has the team's writer write the project's docs from a branch.
pub async fn write(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(FromBranch { branch }): Json<FromBranch>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    let run = start_run(&state, &user, &team_id, &project_id, &branch).await?;
    let job = WriteDocs {
        team_id,
        project_id,
        run,
        branch,
        user_id: user.id,
    };
    jobs::dispatch(&state, Job::WriteDocs(job)).await?;
    Ok(StatusCode::ACCEPTED)
}

/// Has the team's writer answer a person's request with one proposal.
pub async fn ask(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(request): Json<Request>,
) -> Result<StatusCode> {
    user.member_of(&team_id)?;
    if request.text.trim().is_empty() {
        return Err(ApiError::invalid("Say what to change"));
    }
    let run = start_run(&state, &user, &team_id, &project_id, &request.branch).await?;
    let job = AnswerDocsRequest {
        team_id,
        project_id,
        run,
        user_id: user.id,
        request,
    };
    jobs::dispatch(&state, Job::AnswerDocsRequest(job)).await?;
    Ok(StatusCode::ACCEPTED)
}

/// A run that holds the project while the docs are written from `branch`, or why none can start.
async fn start_run(
    state: &AppState,
    user: &User,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> Result<i64> {
    let commit = db::projects::branch_commit(&state.pool, team_id, project_id, branch).await?;
    if commit.ok_or_else(ApiError::not_found)?.is_none() {
        return Err(ApiError::invalid("This branch isn't mapped yet"));
    }
    docs::writer(state, team_id, &user.id).await?;
    let run = rand::random::<u32>().into();
    if !db::docs::claim_run(&state.pool, project_id, run).await? {
        return Err(ApiError::invalid("The docs are already being written"));
    }
    live::project(state, project_id, Topic::Docs).await;
    Ok(run)
}
