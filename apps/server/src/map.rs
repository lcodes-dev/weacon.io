use std::collections::HashMap;
use std::fmt::Display;
use std::io;
use std::io::Read as _;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use code_map::{Reading, key, read};
use flate2::read::GzDecoder;
use serde::Deserialize;

use crate::db::{self, User, Waiting};
use crate::teams::membership;
use crate::{ApiError, AppState, github};

/// Raise this when what the `code-map` crate reads from a file changes, such as with a new language: every map is then read again.
const READER: i64 = 7;

/// Larger files are listed in the map and not read.
const MAX_BYTES: usize = 500_000;

/// How much new text one mapping may keep in memory before the commit is refused.
const MAX_NEW_BYTES: usize = 200_000_000;

/// How much an archive may unpack to, counting the files that are skipped.
const MAX_UNPACKED: u64 = 2_000_000_000;

/// Reads from a commit's archive the files whose content is new, by git's id of it.
fn read_archive(
    archive: &[u8],
    mut new: HashMap<String, String>,
) -> io::Result<Vec<(String, Reading)>> {
    let mut readings = Vec::new();
    let mut kept = 0;
    // One byte past the limit can be read, which tells an archive that is too large from one that ends there.
    let mut unpacked = GzDecoder::new(archive).take(MAX_UNPACKED + 1);
    let mut archive = tar::Archive::new(&mut unpacked);
    for entry in archive.entries()? {
        let mut entry = entry?;
        // Everything in GitHub's archive sits in one folder named after the commit.
        let path: std::path::PathBuf = entry.path()?.components().skip(1).collect();
        let path = path.to_string_lossy().into_owned();
        let Some(blob) = new.remove(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.size() as usize <= MAX_BYTES {
            entry.read_to_end(&mut bytes)?;
        }
        kept += bytes.len();
        if kept > MAX_NEW_BYTES {
            return Err(io::Error::other("it holds too much text to map yet"));
        }
        readings.push((blob, read(&path, &bytes)));
    }
    if unpacked.limit() == 0 {
        return Err(io::Error::other(
            "it unpacks to more than can be mapped yet",
        ));
    }
    // Whatever the archive lacks, such as a link, is recorded so it isn't asked for again.
    readings.extend(new.into_values().map(|blob| (blob, Reading::default())));
    Ok(readings)
}

/// Runs for as long as the server does.
pub async fn worker(state: AppState) {
    if let Err(error) = db::forget_maps_of_other_readers(&state.pool, READER).await {
        tracing::error!("forgetting old maps failed: {error}");
    }
    loop {
        round(&state).await;
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            () = state.map_wake.notified() => {}
        }
    }
}

/// Notes each tracked branch's latest commit, then maps the branches that are behind.
pub async fn round(state: &AppState) {
    if let Err(error) = notice(state).await {
        tracing::error!("noticing commits failed: {error}");
    }
    let waiting = db::waiting_branches(&state.pool).await.unwrap_or_default();
    for branch in waiting {
        if let Err(ApiError(_, message)) = map(state, &branch).await {
            tracing::error!("mapping {} {} failed: {message}", branch.repo, branch.name);
            let (project, name) = (branch.project_id, &branch.name);
            let saved =
                db::set_branch_head(&state.pool, &project, name, None, Some(&message)).await;
            saved.ok();
        }
    }
}

async fn notice(state: &AppState) -> sqlx::Result<()> {
    for project in db::tracked_projects(&state.pool).await? {
        let heads = match github::token(state, &project.team_id, None).await {
            Ok(token) => github::branches(state, &token, &project.repo).await,
            Err(error) => Err(error),
        };
        for name in db::branch_names(&state.pool, &project.id).await? {
            let (head, error) = match &heads {
                Ok(heads) => match heads.get(&name) {
                    Some(head) => (Some(head.as_str()), None),
                    None => (None, Some("This branch isn't on GitHub")),
                },
                Err(ApiError(_, message)) => (None, Some(message.as_str())),
            };
            db::set_branch_head(&state.pool, &project.id, &name, head, error).await?;
        }
    }
    Ok(())
}

async fn map(state: &AppState, branch: &Waiting) -> Result<(), ApiError> {
    let token = github::token(state, &branch.team_id, None).await?;
    let files = github::files(state, &token, &branch.repo, &branch.head_sha).await?;
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, blob)| {
            let key = key(&path, &blob);
            (path, key)
        })
        .collect();
    let mut seen = db::known_blobs(&state.pool, &branch.project_id).await?;
    // One path for each content not read before, even when several files share it.
    let new: HashMap<String, String> = files
        .iter()
        .filter(|(_, blob)| seen.insert(blob.clone()))
        .cloned()
        .collect();
    let mut readings = Vec::new();
    if !new.is_empty() {
        let archive = github::archive(state, &token, &branch.repo, &branch.head_sha).await?;
        // Parsing is slow work that must not hold up the server's other requests.
        let reading = tokio::task::spawn_blocking(move || read_archive(&archive, new));
        // A partial reading would be saved as the whole map, so any failure stops here.
        readings = reading.await.map_err(unread)?.map_err(unread)?;
    }
    db::save_map(&state.pool, branch, &files, &readings).await?;
    Ok(())
}

fn unread(error: impl Display) -> ApiError {
    let message = format!("The commit's archive couldn't be read: {error}");
    ApiError(StatusCode::BAD_GATEWAY, message)
}

#[derive(Deserialize)]
pub struct Which {
    branch: String,
}

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(which): Query<Which>,
) -> Result<Json<db::Map>, ApiError> {
    membership(&user, &team_id)?;
    let map = db::map(&state.pool, &team_id, &project_id, &which.branch).await?;
    Ok(Json(map.ok_or_else(crate::not_found)?))
}
