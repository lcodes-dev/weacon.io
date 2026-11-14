use std::path::PathBuf;

use weacon_codemap::Facts;
use weacon_observability::{Field, Logger};

use super::{Base, RepoMap, Source, content_key};

/// Keeps one worktree's layer of its repository's code map current by polling git, and keeps
/// the base layer's facts in memory between polls.
pub(crate) struct Watch<S> {
    repo: RepoMap<S>,
    worktree: PathBuf,
    base: Option<Base>,
    /// What the worktree's changes were at the last update.
    seen: Option<String>,
    failing: bool,
}

impl<S: Source> Watch<S> {
    pub(crate) const fn new(repo: RepoMap<S>, worktree: PathBuf) -> Self {
        Self {
            repo,
            worktree,
            base: None,
            seen: None,
            failing: false,
        }
    }

    /// Updates the worktree's layer when the base's commit or the worktree's changes moved,
    /// and gives its facts then; `None` when nothing moved or the reading failed, which is
    /// logged once until one succeeds.
    pub(crate) async fn check(&mut self, log: &Logger) -> Option<Vec<Facts>> {
        match self.update().await {
            Ok(facts) => {
                self.failing = false;
                if let Some(facts) = &facts {
                    log.info("map updated", &[("files", facts.len().into())]);
                }
                facts
            }
            Err(error) => {
                if !self.failing {
                    log.warn("map update failed", &[("err", Field::error(&error))]);
                }
                self.failing = true;
                None
            }
        }
    }

    async fn update(&mut self) -> Result<Option<Vec<Facts>>, weacon_observability::Fault> {
        let head = self.repo.head().await?;
        let moved = self.base.as_ref().is_none_or(|base| base.commit != head);
        if moved {
            self.base = Some(self.repo.base().await?);
        }
        let Some(base) = &self.base else {
            return Ok(None);
        };
        let changed = self.repo.changed(base, &self.worktree).await?;
        let mut state = base.commit.clone();
        for (path, text) in &changed {
            let key = text.as_deref().map(content_key).unwrap_or_default();
            state.extend(["\0", path, "\0", &key]);
        }
        let state = content_key(&state);
        if !moved && self.seen.as_deref() == Some(state.as_str()) {
            return Ok(None);
        }
        let facts = self.repo.worktree(base, &self.worktree, changed).await?;
        self.seen = Some(state);
        Ok(Some(facts))
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{RepoId, WorkspaceId};
    use weacon_testing::{TempDir, messages, recording_logger};

    use super::*;
    use crate::codemap::fake::Fake;
    use crate::db::Db;

    const MANIFEST: &str = "[package]\nname = \"a\"\n";

    async fn watch(dir: &TempDir) -> (Watch<Fake>, Fake) {
        let db = Db::open(dir).await.unwrap();
        let workspace = db.workspace(&WorkspaceId::try_from("one".to_owned()).unwrap());
        let map = workspace.map(&RepoId::try_from("repo".to_owned()).unwrap());
        let source = Fake::default();
        let repo = RepoMap {
            map,
            source: source.clone(),
        };
        (Watch::new(repo, PathBuf::from("/w/chat")), source)
    }

    #[tokio::test]
    async fn updates_only_when_the_base_or_the_worktree_moved() {
        let dir = TempDir::new().unwrap();
        let (mut watch, source) = watch(&dir).await;
        let (log, records) = recording_logger();
        source.commit(
            "c1",
            &[("Cargo.toml", MANIFEST), ("src/lib.rs", "fn a() {}\n")],
        );
        assert_eq!(watch.check(&log).await.map(|facts| facts.len()), Some(2));
        assert_eq!(watch.check(&log).await, None);

        source.change(&[("src/new.rs", Some("fn b() {}\n"))]);
        assert_eq!(watch.check(&log).await.map(|facts| facts.len()), Some(3));
        assert_eq!(watch.check(&log).await, None);
        source.change(&[("src/new.rs", Some("fn c() {}\n"))]);
        assert!(watch.check(&log).await.is_some());

        source.commit(
            "c2",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", "fn a() {}\nfn d() {}\n"),
            ],
        );
        assert!(watch.check(&log).await.is_some());
        assert_eq!(messages(&records), ["map updated"; 4]);
    }

    #[tokio::test]
    async fn warns_once_while_reading_fails() {
        let dir = TempDir::new().unwrap();
        let (mut watch, source) = watch(&dir).await;
        let (log, records) = recording_logger();
        source.commit("c1", &[("Cargo.toml", MANIFEST)]);
        source.fail(true);
        assert_eq!(watch.check(&log).await, None);
        assert_eq!(watch.check(&log).await, None);
        source.fail(false);
        assert!(watch.check(&log).await.is_some());
        source.fail(true);
        assert_eq!(watch.check(&log).await, None);
        let expected = ["map update failed", "map updated", "map update failed"];
        assert_eq!(messages(&records), expected);
    }
}
