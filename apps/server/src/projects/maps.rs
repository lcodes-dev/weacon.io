use std::collections::HashMap;
use std::fmt::Display;
use std::io;
use std::io::Read as _;

use code_map::Reading;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use super::db::Waiting;
use super::{db, github};
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::{AppState, live};

pub const READER: i64 = 7;

const MAX_BYTES: usize = 500_000;

const MAX_NEW_BYTES: usize = 200_000_000;

const MAX_UNPACKED: u64 = 2_000_000_000;

pub async fn notice_commits(state: &AppState) -> Result<()> {
    for project in db::tracked(&state.pool).await? {
        let heads = match github::token(state, &project.team_id, None, github::reading()).await {
            Ok(token) => github::branches(state, &token, &project.repo).await,
            Err(error) => Err(error),
        };
        for name in db::branch_names(&state.pool, &project.id).await? {
            let (head, error) = match &heads {
                Ok(heads) => match heads.get(&name) {
                    Some(head) => (Some(head.as_str()), None),
                    None => (None, Some("This branch isn't on GitHub")),
                },
                Err(error) => (None, Some(error.message.as_str())),
            };
            if db::set_branch_head(&state.pool, &project.id, &name, head, error).await? {
                live::team(state, &project.team_id, Topic::Projects).await;
            }
        }
    }
    Ok(())
}

pub async fn map_branch(state: &AppState, branch: &Waiting) {
    if let Err(error) = map(state, branch).await {
        let (repo, name) = (&branch.repo, &branch.name);
        tracing::error!("mapping {repo} {name} failed: {}", error.message);
        let failed = Some(error.message.as_str());
        let saved = db::set_branch_head(&state.pool, &branch.project_id, name, None, failed);
        saved.await.ok();
        live::team(state, &branch.team_id, Topic::Projects).await;
    }
}

async fn map(state: &AppState, branch: &Waiting) -> Result<()> {
    let token = github::token(state, &branch.team_id, None, github::reading()).await?;
    let files = github::files(state, &token, &branch.repo, &branch.head_sha).await?;
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, blob)| {
            let key = code_map::key(&path, &blob);
            (path, key)
        })
        .collect();
    let mut seen = db::known_blobs(&state.pool, &branch.project_id).await?;
    let new: HashMap<String, String> = files
        .iter()
        .filter(|(_, blob)| seen.insert(blob.clone()))
        .cloned()
        .collect();
    let mut readings = Vec::new();
    if !new.is_empty() {
        let archive = github::archive(state, &token, &branch.repo, &branch.head_sha).await?;
        let reading = tokio::task::spawn_blocking(move || read_archive(&archive, new));
        readings = reading.await.map_err(unread)?.map_err(unread)?;
    }
    db::save_map(&state.pool, branch, &files, &readings).await?;
    live::team(state, &branch.team_id, Topic::Projects).await;
    Ok(())
}

fn unread(error: impl Display) -> ApiError {
    ApiError::bad_gateway(format!("The commit's archive couldn't be read: {error}"))
}

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
        readings.push((blob, code_map::read(&path, &bytes)));
    }
    if unpacked.limit() == 0 {
        return Err(io::Error::other(
            "it unpacks to more than can be mapped yet",
        ));
    }
    readings.extend(new.into_values().map(|blob| (blob, Reading::default())));
    Ok(readings)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MapBranches;

impl MapBranches {
    pub const KEY: &str = "map_branches";

    pub async fn perform(self, state: &AppState) -> Result<()> {
        if let Err(error) = notice_commits(state).await {
            tracing::error!("noticing commits failed: {}", error.message);
        }
        for branch in db::waiting(&state.pool).await? {
            map_branch(state, &branch).await;
        }
        Ok(())
    }
}
