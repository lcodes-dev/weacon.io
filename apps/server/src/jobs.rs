use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::{error, info, warn};

use crate::chats::StartTurn;
use crate::db::new_id;
use crate::docs::jobs::{AnswerDocsRequest, CheckPage, WriteDocs};
use crate::error::{ApiError, Result};
use crate::groups::AnswerAsWeacon;
use crate::projects::maps::MapBranches;
use crate::{AppState, now};

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
    fn name(&self) -> &'static str {
        match self {
            Job::MapBranches(_) => "MapBranches",
            Job::WriteDocs(_) => "WriteDocs",
            Job::AnswerDocsRequest(_) => "AnswerDocsRequest",
            Job::CheckPage(_) => "CheckPage",
            Job::AnswerAsWeacon(_) => "AnswerAsWeacon",
            Job::StartTurn(_) => "StartTurn",
        }
    }

    fn unique_key(&self) -> Option<String> {
        match self {
            Job::MapBranches(_) => Some(MapBranches::KEY.into()),
            Job::AnswerAsWeacon(job) => Some(AnswerAsWeacon::key(&job.group_id)),
            _ => None,
        }
    }

    // A call to @weacon may have made a task before it failed, and the map runs every minute anyway.
    fn attempts(&self) -> i64 {
        match self {
            Job::MapBranches(_) | Job::AnswerAsWeacon(_) => 1,
            _ => 3,
        }
    }

    async fn perform(self, state: &AppState) -> Result<()> {
        match self {
            Job::MapBranches(job) => job.perform(state).await,
            Job::WriteDocs(job) => job.perform(state).await,
            Job::AnswerDocsRequest(job) => job.perform(state).await,
            Job::CheckPage(job) => job.perform(state).await,
            Job::AnswerAsWeacon(job) => job.perform(state).await,
            Job::StartTurn(job) => job.perform(state).await,
        }
    }

    async fn failed(self, state: &AppState, why: String) {
        match self {
            Job::MapBranches(_) | Job::StartTurn(_) => {}
            Job::WriteDocs(job) => job.failed(state, why).await,
            Job::AnswerDocsRequest(job) => job.failed(state, why).await,
            Job::CheckPage(job) => job.failed(state).await,
            Job::AnswerAsWeacon(job) => job.failed(state, why).await,
        }
    }
}

pub async fn perform_later(state: &AppState, job: Job) -> sqlx::Result<bool> {
    let row = serde_json::to_string(&job).expect("a job is plain data");
    let Some(id) = enqueue(&state.pool, &row, job.unique_key().as_deref()).await? else {
        return Ok(false);
    };
    info!(job = job.name(), id, "enqueued");
    tokio::spawn(run(state.clone(), id, job, Duration::ZERO));
    Ok(true)
}

pub async fn resume(state: &AppState) -> sqlx::Result<()> {
    for queued in all(&state.pool).await? {
        let Ok(job) = serde_json::from_str::<Job>(&queued.job) else {
            delete(state, &queued.id).await;
            continue;
        };
        if queued.started {
            let restarted = ApiError::internal("The server restarted");
            if let Some(wait) = retry_or_fail(state, &queued.id, &job, queued.attempts, restarted).await {
                tokio::spawn(run(state.clone(), queued.id, job, wait));
            }
        } else {
            let wait = Duration::from_secs((queued.run_at - now()).max(0) as u64);
            tokio::spawn(run(state.clone(), queued.id, job, wait));
        }
    }
    Ok(())
}

pub async fn schedule(state: AppState) {
    let mut every_minute = tokio::time::interval(Duration::from_secs(60));
    loop {
        every_minute.tick().await;
        if let Err(why) = perform_later(&state, Job::MapBranches(MapBranches)).await {
            error!(job = "MapBranches", "couldn't be enqueued: {why}");
        }
    }
}

async fn run(state: AppState, id: String, job: Job, mut wait: Duration) {
    loop {
        tokio::time::sleep(wait).await;
        match attempt(&state, &id, &job).await {
            Some(next) => wait = next,
            None => return,
        }
    }
}

async fn attempt(state: &AppState, id: &str, job: &Job) -> Option<Duration> {
    let attempts = match start(&state.pool, id).await {
        Ok(attempts) => attempts,
        Err(why) => {
            error!(job = job.name(), id, "couldn't start: {why}");
            return None;
        }
    };
    info!(job = job.name(), id, attempts, "performing");
    let started = Instant::now();
    let performed = tokio::spawn({
        let (state, job) = (state.clone(), job.clone());
        async move { job.perform(&state).await }
    });
    match performed.await {
        Ok(Ok(())) => {
            let ms = started.elapsed().as_millis() as u64;
            info!(job = job.name(), id, ms, "performed");
            delete(state, id).await;
            None
        }
        Ok(Err(why)) => retry_or_fail(state, id, job, attempts, why).await,
        Err(_) => {
            fail(state, id, job, attempts, ApiError::internal("It panicked")).await;
            None
        }
    }
}

async fn retry_or_fail(
    state: &AppState,
    id: &str,
    job: &Job,
    attempts: i64,
    why: ApiError,
) -> Option<Duration> {
    if !why.status.is_server_error() || attempts >= job.attempts() {
        fail(state, id, job, attempts, why).await;
        return None;
    }
    let wait = state.retry_wait * (attempts.pow(4) + 2) as u32;
    warn!(job = job.name(), id, attempts, "retrying in {wait:?}: {}", why.message);
    let run_at = now() + wait.as_secs() as i64;
    match retry(&state.pool, id, run_at, &why.message).await {
        Ok(()) => Some(wait),
        Err(error) => {
            error!(job = job.name(), id, "couldn't be retried: {error}");
            None
        }
    }
}

async fn fail(state: &AppState, id: &str, job: &Job, attempts: i64, why: ApiError) {
    error!(job = job.name(), id, attempts, "failed: {}", why.message);
    job.clone().failed(state, why.message).await;
    delete(state, id).await;
}

async fn delete(state: &AppState, id: &str) {
    let deleted = sqlx::query!("DELETE FROM jobs WHERE id = ?", id).execute(&state.pool).await;
    if let Err(why) = deleted {
        error!(id, "couldn't be deleted: {why}");
    }
}

pub struct Queued {
    pub id: String,
    pub job: String,
    pub attempts: i64,
    pub run_at: i64,
    pub started: bool,
}

async fn enqueue(pool: &SqlitePool, job: &str, unique_key: Option<&str>) -> sqlx::Result<Option<String>> {
    let id = new_id();
    let added = sqlx::query!(
        "INSERT OR IGNORE INTO jobs (id, job, unique_key, run_at, created_at)
         VALUES (?, ?, ?, unixepoch(), unixepoch())",
        id,
        job,
        unique_key
    )
    .execute(pool)
    .await?;
    Ok((added.rows_affected() > 0).then_some(id))
}

async fn start(pool: &SqlitePool, id: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar!(
        "UPDATE jobs SET started_at = unixepoch(), attempts = attempts + 1 WHERE id = ? RETURNING attempts",
        id
    )
    .fetch_one(pool)
    .await
}

async fn retry(pool: &SqlitePool, id: &str, run_at: i64, why: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE jobs SET started_at = NULL, run_at = ?, last_error = ? WHERE id = ?",
        run_at,
        why,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn all(pool: &SqlitePool) -> sqlx::Result<Vec<Queued>> {
    sqlx::query_as!(
        Queued,
        r#"SELECT id, job, attempts, run_at, started_at IS NOT NULL AS "started!: bool"
           FROM jobs ORDER BY id"#
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
