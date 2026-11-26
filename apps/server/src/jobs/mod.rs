//! Work done after a request is answered. A job is kept in the `jobs` table from when it is
//! dispatched until it has run, so that a restart neither loses it nor leaves it half done.

mod answer_as_weacon;
pub mod answer_docs_request;
mod check_page;
mod map_branches;
mod start_turn;
mod write_docs;

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use answer_as_weacon::AnswerAsWeacon;
pub use answer_docs_request::AnswerDocsRequest;
pub use check_page::CheckPage;
pub use map_branches::MapBranches;
pub use start_turn::StartTurn;
pub use write_docs::WriteDocs;

use crate::error::Result;
use crate::{AppState, db};

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

    /// Ends what the job left undone, so that nothing shows as still running.
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

/// Queues the job and starts it. False when a job with the same key is queued already.
pub async fn dispatch(state: &AppState, job: Job) -> sqlx::Result<bool> {
    let queued = serde_json::to_string(&job).expect("a job is plain data");
    let Some(id) = db::jobs::add(&state.pool, &queued, job.unique_key().as_deref()).await? else {
        return Ok(false);
    };
    tokio::spawn(run(state.clone(), id, job));
    Ok(true)
}

/// At startup: a job the restart cut short fails, and one that never started runs now.
pub async fn resume(state: &AppState) -> sqlx::Result<()> {
    for queued in db::jobs::all(&state.pool).await? {
        let Ok(job) = serde_json::from_str::<Job>(&queued.job) else {
            db::jobs::delete(&state.pool, &queued.id).await?;
            continue;
        };
        if queued.started {
            job.failed(state, "The server restarted".into()).await;
            db::jobs::delete(&state.pool, &queued.id).await?;
        } else {
            tokio::spawn(run(state.clone(), queued.id, job));
        }
    }
    Ok(())
}

/// Dispatches the jobs that run on the clock, for as long as the server runs.
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
    if let Err(error) = db::jobs::start(&state.pool, &id).await {
        tracing::error!("a job couldn't start: {error}");
    }
    // In a task of its own, so that a job that panics still fails as it should.
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
    if let Err(error) = db::jobs::delete(&state.pool, &id).await {
        tracing::error!("a finished job couldn't be deleted: {error}");
    }
}
