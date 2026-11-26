use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::chats::StartTurn;
use crate::db::new_id;
use crate::docs::jobs::{AnswerDocsRequest, CheckPage, WriteDocs};
use crate::error::Result;
use crate::groups::AnswerAsWeacon;
use crate::projects::maps::MapBranches;
use crate::{AppState, jobs};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Job {
    MapBranches(MapBranches),
    WriteDocs(WriteDocs),
    AnswerDocsRequest(AnswerDocsRequest),
    CheckPage(CheckPage),
    AnswerAsWeacon(AnswerAsWeacon),
    StartTurn(StartTurn),
}

impl Job {
    fn unique_key(&self) -> Option<String> {
        match self {
            Job::MapBranches(_) => Some(MapBranches::KEY.into()),
            Job::AnswerAsWeacon(job) => Some(AnswerAsWeacon::key(&job.group_id)),
            _ => None,
        }
    }

    async fn handle(self, state: &AppState) -> Result<()> {
        match self {
            Job::MapBranches(job) => job.handle(state).await,
            Job::WriteDocs(job) => job.handle(state).await,
            Job::AnswerDocsRequest(job) => job.handle(state).await,
            Job::CheckPage(job) => job.handle(state).await,
            Job::AnswerAsWeacon(job) => job.handle(state).await,
            Job::StartTurn(job) => job.handle(state).await,
        }
    }

    async fn failed(self, state: &AppState, why: String) {
        tracing::warn!("a job failed: {why}");
        match self {
            Job::MapBranches(_) | Job::StartTurn(_) => {}
            Job::WriteDocs(job) => job.failed(state, why).await,
            Job::AnswerDocsRequest(job) => job.failed(state, why).await,
            Job::CheckPage(job) => job.failed(state).await,
            Job::AnswerAsWeacon(job) => job.failed(state, why).await,
        }
    }
}

pub async fn dispatch(state: &AppState, job: Job) -> sqlx::Result<bool> {
    let queued = serde_json::to_string(&job).expect("a job is plain data");
    let Some(id) = jobs::add(&state.pool, &queued, job.unique_key().as_deref()).await? else {
        return Ok(false);
    };
    tokio::spawn(run(state.clone(), id, job));
    Ok(true)
}

pub async fn resume(state: &AppState) -> sqlx::Result<()> {
    for queued in jobs::all(&state.pool).await? {
        let Ok(job) = serde_json::from_str::<Job>(&queued.job) else {
            jobs::delete(&state.pool, &queued.id).await?;
            continue;
        };
        if queued.started {
            job.failed(state, "The server restarted".into()).await;
            jobs::delete(&state.pool, &queued.id).await?;
        } else {
            tokio::spawn(run(state.clone(), queued.id, job));
        }
    }
    Ok(())
}

pub async fn schedule(state: AppState) {
    let mut every_minute = tokio::time::interval(Duration::from_secs(60));
    loop {
        every_minute.tick().await;
        let mapping = dispatch(&state, Job::MapBranches(MapBranches)).await;
        if let Err(error) = mapping {
            tracing::error!("the branches couldn't be queued for mapping: {error}");
        }
    }
}

async fn run(state: AppState, id: String, job: Job) {
    if let Err(error) = jobs::start(&state.pool, &id).await {
        tracing::error!("a job couldn't start: {error}");
    }
    let handled = tokio::spawn({
        let (state, job) = (state.clone(), job.clone());
        async move { job.handle(&state).await }
    });
    let failure = match handled.await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.message),
        Err(_) => Some("Something went wrong".into()),
    };
    if let Some(why) = failure {
        job.failed(&state, why).await;
    }
    if let Err(error) = jobs::delete(&state.pool, &id).await {
        tracing::error!("a finished job couldn't be deleted: {error}");
    }
}

pub struct Queued {
    pub id: String,
    pub job: String,
    pub started: bool,
}

pub async fn add(
    pool: &SqlitePool,
    job: &str,
    unique_key: Option<&str>,
) -> sqlx::Result<Option<String>> {
    let id = new_id();
    let added = sqlx::query!(
        "INSERT OR IGNORE INTO jobs (id, job, unique_key, created_at) VALUES (?, ?, ?, unixepoch())",
        id,
        job,
        unique_key
    )
    .execute(pool)
    .await?;
    Ok((added.rows_affected() > 0).then_some(id))
}

pub async fn start(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE jobs SET started_at = unixepoch() WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM jobs WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn all(pool: &SqlitePool) -> sqlx::Result<Vec<Queued>> {
    sqlx::query_as!(
        Queued,
        r#"SELECT id, job, started_at IS NOT NULL AS "started!: bool" FROM jobs ORDER BY id"#
    )
    .fetch_all(pool)
    .await
}

pub async fn exists(pool: &SqlitePool, unique_key: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM jobs WHERE unique_key = ?) AS "found: bool""#,
        unique_key
    )
    .fetch_one(pool)
    .await
}
