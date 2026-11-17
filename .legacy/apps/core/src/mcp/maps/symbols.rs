//! Search through the items of each repository a workspace registered, at its base.

use std::path::PathBuf;

use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{RepoId, WorkspaceId, code};

use super::{Maps, Stores, failed};
use crate::codemap::{locate, off_runtime, repo_map};
use crate::db::{Hit, Looking};
use crate::git::state::Stores as GitStores;

impl Maps {
    /// What search finds for `words` among the items of each repository `workspace`
    /// registered, at its main worktree's commit, best first in each; one that can't be read is
    /// left out. A base read past the limit goes on, so the next search finds it stored.
    pub(crate) async fn symbols(
        &self,
        workspace: &WorkspaceId,
        words: &str,
        log: &Logger,
    ) -> Result<Vec<(RepoId, Vec<Hit>)>, Fault> {
        let stores = self.stores(log).await.ok_or_else(failed)?;
        let (workspace, words, log) = (workspace.clone(), words.to_owned(), log.clone());
        let reading = tokio::spawn(async move { stores.symbols(workspace, &words, &log).await });
        match tokio::time::timeout(self.limit, reading).await {
            Err(_) => Err(still_reading()),
            Ok(Ok(found)) => found,
            Ok(Err(error)) => Err(failed().because(error)),
        }
    }
}

impl Stores {
    /// What [`Maps::symbols`] finds, reading a repository's base when it must.
    async fn symbols(
        &self,
        workspace: WorkspaceId,
        words: &str,
        log: &Logger,
    ) -> Result<Vec<(RepoId, Vec<Hit>)>, Fault> {
        let (listing, asked) = (self.clone(), workspace.clone());
        let repos = off_runtime(move || listing.mains(&asked)).await?;
        let store = self.db.workspace(&workspace);
        let mut found = Vec::new();
        for (repo, main) in repos {
            let map = repo_map(&store, &self.git, main);
            match map.search_head(words, Looking::Items).await {
                Ok(hits) => found.push((repo, hits)),
                Err(fault) => {
                    let about = [
                        ("repo", repo.as_str().into()),
                        ("err", Field::error(&fault)),
                    ];
                    log.warn("symbols repo failed", &about);
                }
            }
        }
        Ok(found)
    }

    /// The id and main worktree of each repository `workspace` registered that is one here,
    /// once each; it blocks.
    fn mains(&self, workspace: &WorkspaceId) -> Result<Vec<(RepoId, PathBuf)>, Fault> {
        let repos = GitStores::new(&self.workspaces, workspace).repos().list()?;
        let mut mains: Vec<(RepoId, PathBuf)> = Vec::new();
        for repo in repos {
            let Ok((main, _)) = locate(&self.git, &repo.path) else {
                continue;
            };
            if !mains.iter().any(|(_, known)| *known == main) {
                mains.push((repo.id, main));
            }
        }
        Ok(mains)
    }
}

fn still_reading() -> Fault {
    Fault::new(
        code!("symbols.still_reading"),
        "The code map is still being read",
    )
}
