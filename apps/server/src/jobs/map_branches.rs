use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::{AppState, db, maps};

/// Notes each tracked branch's latest commit, then maps the branches that are behind. It runs
/// every minute, and when a project's branches change.
#[derive(Clone, Serialize, Deserialize)]
pub struct MapBranches;

impl MapBranches {
    pub const KEY: &str = "map_branches";

    pub async fn handle(self, state: &AppState) -> Result<()> {
        if let Err(error) = maps::notice_commits(state).await {
            tracing::error!("noticing commits failed: {}", error.message);
        }
        for branch in db::projects::waiting(&state.pool).await? {
            maps::map_branch(state, &branch).await;
        }
        Ok(())
    }
}
