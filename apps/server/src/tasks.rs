use crate::db::tasks::TaskFields;
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db};

pub const STATUSES: [&str; 3] = ["todo", "doing", "done"];

pub async fn create(
    state: &AppState,
    (team_id, project_id): (&str, &str),
    user_id: &str,
    chat_id: Option<&str>,
    fields: TaskFields,
) -> Result<(String, i64)> {
    let fields = checked(state, team_id, fields).await?;
    if fields.title.is_none() {
        return Err(ApiError::invalid("A task needs a title"));
    }
    let task = db::tasks::create(&state.pool, project_id, user_id, chat_id, &fields).await?;
    live::team(state, team_id, Topic::Tasks).await;
    Ok(task)
}

pub async fn update(
    state: &AppState,
    (team_id, project_id): (&str, &str),
    task_id: &str,
    fields: TaskFields,
) -> Result<TaskFields> {
    let fields = checked(state, team_id, fields).await?;
    db::tasks::change(&state.pool, project_id, task_id, &fields).await?;
    live::team(state, team_id, Topic::Tasks).await;
    Ok(fields)
}

pub fn check_status(status: &str) -> Result<()> {
    if !STATUSES.contains(&status) {
        return Err(ApiError::invalid("A task is todo, doing or done"));
    }
    Ok(())
}

async fn checked(state: &AppState, team_id: &str, mut fields: TaskFields) -> Result<TaskFields> {
    if let Some(title) = &mut fields.title {
        *title = title.trim().into();
        if title.is_empty() {
            return Err(ApiError::invalid("A task needs a title"));
        }
    }
    if let Some(checks) = &mut fields.checks {
        checks.retain(|check| !check.text.trim().is_empty());
    }
    if let Some(status) = &fields.status {
        check_status(status)?;
    }
    if let Some(Some(owner)) = &fields.owner_id {
        let members = db::teams::member_ids(&state.pool, team_id).await?;
        if !members.contains(owner) {
            return Err(ApiError::invalid("A task's owner is a member of the team"));
        }
    }
    Ok(fields)
}
