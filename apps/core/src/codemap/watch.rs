use std::path::PathBuf;

use weacon_codemap::Facts;
use weacon_observability::{Fault, Field, Logger};

use super::{Base, RepoMap, Source};
use crate::db::BASE;

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

    async fn update(&mut self) -> Result<Option<Vec<Facts>>, Fault> {
        let head = self.repo.head().await?;
        let stored = self.repo.map.commit_of(BASE).await?;
        let base = match self.base.take() {
            Some(base) if base.commit == head && stored.as_deref() == Some(head.as_str()) => base,
            // Another refresh may have stored a base meanwhile, so this worktree's layer is rebuilt.
            _ => {
                self.seen = None;
                self.repo.base().await?
            }
        };
        let base = self.base.insert(base);
        let changed = self.repo.changed(base, &self.worktree).await?;
        if self.seen.as_deref() == Some(changed.state.as_str()) {
            return Ok(None);
        }
        let state = changed.state.clone();
        let facts = self.repo.worktree(base, &self.worktree, changed).await?;
        self.seen = Some(state);
        Ok(Some(facts))
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::WorkspaceId;
    use weacon_testing::{TempDir, messages, recording_logger};

    use super::*;
    use crate::codemap::fake::{Fake, commit_id};
    use crate::db::Db;

    const MANIFEST: &str = "[package]\nname = \"a\"\n";

    async fn watch(dir: &TempDir) -> (Watch<Fake>, Fake) {
        let db = Db::open(dir).await.unwrap();
        let workspace = db.workspace(&WorkspaceId::try_from("one".to_owned()).unwrap());
        let map = workspace.map(&crate::db::LocalRepo::at(std::path::Path::new("/repo")));
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
        source.heads_read();
        assert_eq!(watch.check(&log).await, None);
        assert_eq!(source.heads_read(), 1, "the base was read again");

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
    async fn stores_its_base_again_when_another_refresh_replaced_it() {
        let dir = TempDir::new().unwrap();
        let (mut watch, source) = watch(&dir).await;
        let (log, _) = recording_logger();
        let first = [("Cargo.toml", MANIFEST), ("src/lib.rs", "fn a() {}\n")];
        source.commit("c1", &first);
        assert!(watch.check(&log).await.is_some());

        let other = watch.repo.clone();
        source.commit("c2", &[("Cargo.toml", MANIFEST)]);
        other.base().await.unwrap();
        source.commit("c1", &first);
        assert_eq!(watch.check(&log).await.map(|facts| facts.len()), Some(2));
        let stored = watch.repo.map.commit_of(BASE).await.unwrap();
        assert_eq!(stored.as_deref(), Some(commit_id("c1").as_str()));
        assert_eq!(watch.check(&log).await, None);
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
