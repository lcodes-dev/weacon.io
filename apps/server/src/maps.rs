//! A tracked branch's map: every file at its latest commit, what each declares, and which
//! file uses which. The `code-map` crate reads the files; this fetches them from GitHub and
//! reads only the contents it hasn't read before.

use std::collections::HashMap;
use std::fmt::Display;
use std::io::{self, Read as _};

use code_map::Reading;
use flate2::read::GzDecoder;

use crate::db::projects::Waiting;
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db, github};

/// Raise this when what the `code-map` crate reads from a file changes, such as with a new
/// language: every map is then read again.
pub const READER: i64 = 7;

/// Larger files are listed in the map and not read.
const MAX_BYTES: usize = 500_000;

/// How much new text one mapping may keep in memory before the commit is refused.
const MAX_NEW_BYTES: usize = 200_000_000;

/// How much an archive may unpack to, counting the files that are skipped.
const MAX_UNPACKED: u64 = 2_000_000_000;

/// Notes the latest commit of each tracked branch, as GitHub has it.
pub async fn notice_commits(state: &AppState) -> Result<()> {
    for project in db::projects::tracked(&state.pool).await? {
        let heads = match github::token(state, &project.team_id, None, github::reading()).await {
            Ok(token) => github::branches(state, &token, &project.repo).await,
            Err(error) => Err(error),
        };
        for name in db::projects::branch_names(&state.pool, &project.id).await? {
            let (head, error) = match &heads {
                Ok(heads) => match heads.get(&name) {
                    Some(head) => (Some(head.as_str()), None),
                    None => (None, Some("This branch isn't on GitHub")),
                },
                Err(error) => (None, Some(error.message.as_str())),
            };
            if db::projects::set_branch_head(&state.pool, &project.id, &name, head, error).await? {
                live::team(state, &project.team_id, Topic::Projects).await;
            }
        }
    }
    Ok(())
}

/// Maps a branch at its latest commit, or records why it couldn't be.
pub async fn map_branch(state: &AppState, branch: &Waiting) {
    if let Err(error) = map(state, branch).await {
        let (repo, name) = (&branch.repo, &branch.name);
        tracing::error!("mapping {repo} {name} failed: {}", error.message);
        let failed = Some(error.message.as_str());
        let saved =
            db::projects::set_branch_head(&state.pool, &branch.project_id, name, None, failed);
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
    let mut seen = db::maps::known_blobs(&state.pool, &branch.project_id).await?;
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
    db::maps::save(&state.pool, branch, &files, &readings).await?;
    live::team(state, &branch.team_id, Topic::Projects).await;
    Ok(())
}

fn unread(error: impl Display) -> ApiError {
    ApiError::bad_gateway(format!("The commit's archive couldn't be read: {error}"))
}

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
        readings.push((blob, code_map::read(&path, &bytes)));
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
