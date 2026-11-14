//! The code map of each repository a workspace registered: a base layer at the main worktree's
//! commit, and a layer per worktree with what it changed, kept in the core's database. It
//! never leaves this machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use weacon_codemap::{Facts, Reader, SourceFile};
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::db::{BASE, Layer, Map, Stored};

/// What the code map reads from a repository. Every call may block.
pub(crate) trait Source: Clone + Send + Sync + 'static {
    /// The commit the main worktree is at.
    fn head(&self) -> Result<String, Fault>;
    /// Each file at `commit`, with its blob.
    fn tree(&self, commit: &str) -> Result<Vec<(String, String)>, Fault>;
    /// The text of each blob that is UTF-8, by blob.
    fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault>;
    /// Each file that differs in the worktree at `path` from `commit`, committed or not, with
    /// its text now, `None` once deleted or when it isn't UTF-8.
    fn changed(&self, path: &Path, commit: &str) -> Result<Vec<(String, Option<String>)>, Fault>;
}

/// One repository's code map, read from `source` and kept in `map`.
#[derive(Clone, Debug)]
pub(crate) struct RepoMap<S> {
    pub(crate) map: Map,
    pub(crate) source: S,
}

impl<S: Source> RepoMap<S> {
    /// The facts of every file at the main worktree's commit, reading only files whose blob
    /// the map hasn't seen.
    pub(crate) async fn base(&self) -> Result<(String, Vec<Facts>), Fault> {
        let commit = self.blocking(|source| source.head()).await?;
        if let Some(layer) = self.map.layer(BASE).await?
            && layer.commit == commit
        {
            return Ok((commit, self.stored_facts(&layer.files).await?));
        }
        let wanted = {
            let commit = commit.clone();
            self.blocking(move |source| source.tree(&commit)).await?
        };
        let files: Vec<Stored> = wanted
            .into_iter()
            .filter(|(path, _)| is_mapped(path))
            .map(|(path, blob)| Stored {
                path,
                blob: Some(blob),
            })
            .collect();
        let (mut facts, missing) = self.cached(&files).await?;
        let blobs: Vec<String> = missing.iter().map(|(_, blob)| blob.clone()).collect();
        let texts = self.blocking(move |source| source.blobs(&blobs)).await?;
        let read = read(missing.into_iter().filter_map(|(path, blob)| {
            let text = texts.get(&blob)?.clone();
            Some((path, blob, text))
        }))?;
        facts.extend(read.iter().map(|(_, _, facts)| facts.clone()));
        let layer = Layer {
            name: BASE.to_owned(),
            commit: commit.clone(),
            files,
        };
        let entries = weacon_codemap::assemble(&facts).entries();
        self.map.replace(&layer, &read, &entries).await?;
        Ok((commit, facts))
    }

    /// The facts of every file as the worktree at `path` has it: the base's, with what the
    /// worktree changed in their place. Only text the map hasn't seen is read.
    pub(crate) async fn worktree(&self, path: &Path) -> Result<Vec<Facts>, Fault> {
        let (commit, base) = self.base().await?;
        let changed = {
            let path = path.to_owned();
            self.blocking(move |source| source.changed(&path, &commit))
                .await?
        };
        let changed: Vec<(String, Option<String>)> = changed
            .into_iter()
            .filter(|(path, _)| is_mapped(path))
            .collect();
        let files: Vec<Stored> = changed
            .iter()
            .map(|(path, text)| Stored {
                path: path.clone(),
                blob: text.as_deref().map(content_key),
            })
            .collect();
        let (mut overlay, missing) = self.cached(&files).await?;
        let missing: BTreeSet<(String, String)> = missing.into_iter().collect();
        let read = read(changed.into_iter().filter_map(|(path, text)| {
            let text = text?;
            let key = content_key(&text);
            missing
                .contains(&(path.clone(), key.clone()))
                .then_some((path, key, text))
        }))?;
        overlay.extend(read.iter().map(|(_, _, facts)| facts.clone()));
        let shadowed: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
        let mut facts: Vec<Facts> = base
            .into_iter()
            .filter(|facts| !shadowed.contains(facts.path()))
            .collect();
        facts.extend(overlay);
        let entries: Vec<_> = weacon_codemap::assemble(&facts)
            .entries()
            .into_iter()
            .filter(|entry| shadowed.contains(entry.file.as_str()))
            .collect();
        let layer = Layer {
            name: layer_name(path),
            commit: String::new(),
            files,
        };
        self.map.replace(&layer, &read, &entries).await?;
        Ok(facts)
    }

    /// The stored facts of `files`, and the files with text that has none yet.
    async fn cached(&self, files: &[Stored]) -> Result<(Vec<Facts>, Vec<(String, String)>), Fault> {
        let wanted: Vec<(String, String)> = files
            .iter()
            .filter_map(|file| Some((file.path.clone(), file.blob.clone()?)))
            .collect();
        let mut found = self.map.facts(&wanted).await?;
        let mut missing = Vec::new();
        let mut facts = Vec::new();
        for key in wanted {
            match found.remove(&key) {
                Some(stored) => facts.push(stored),
                None => missing.push(key),
            }
        }
        Ok((facts, missing))
    }

    async fn stored_facts(&self, files: &[Stored]) -> Result<Vec<Facts>, Fault> {
        let (facts, _) = self.cached(files).await?;
        Ok(facts)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        call: impl FnOnce(&S) -> Result<T, Fault> + Send + 'static,
    ) -> Result<T, Fault> {
        let source = self.source.clone();
        tokio::task::spawn_blocking(move || call(&source))
            .await
            .map_err(|error| {
                Fault::new(code!("map.read_failed"), "The repository couldn't be read")
                    .because(error)
            })?
    }
}

/// The facts of each `(path, key, text)`, with the path and key they're stored under; text
/// that adds nothing, such as a broken manifest, gives none.
fn read(
    texts: impl Iterator<Item = (String, String, String)>,
) -> Result<Vec<(String, String, Facts)>, Fault> {
    let mut reader = Reader::new().map_err(index_failed)?;
    let mut found = Vec::new();
    for (path, key, text) in texts {
        let file = SourceFile {
            path: path.clone(),
            text,
        };
        if let Some(facts) = reader.facts(&file).map_err(index_failed)? {
            found.push((path, key, facts));
        }
    }
    Ok(found)
}

fn index_failed(error: weacon_codemap::Error) -> Fault {
    Fault::new(code!("map.index_failed"), "The code map couldn't be built").because(error)
}

/// Every `Cargo.toml`, `.rs` file inside a `src` folder and Markdown file.
fn is_mapped(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default();
    let in_src = path.split('/').any(|part| part == "src");
    name == "Cargo.toml" || name.ends_with(".md") || (in_src && name.ends_with(".rs"))
}

/// What a worktree's text is stored under: its hash, which no git blob id looks like.
fn content_key(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// The layer that holds what the worktree at `path` changed.
fn layer_name(path: &Path) -> String {
    format!("worktree:{}", PathBuf::from(path).display())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use weacon_protocol::{RepoId, WorkspaceId};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;

    /// A repository in memory: commits by id, each its files' text by path.
    #[derive(Clone, Default)]
    struct Fake {
        state: Arc<Mutex<State>>,
    }

    #[derive(Default)]
    struct State {
        head: String,
        commits: BTreeMap<String, BTreeMap<String, String>>,
        changed: Vec<(String, Option<String>)>,
        blobs_read: Vec<String>,
    }

    fn blob(text: &str) -> String {
        format!("blob-{}", content_key(text))
    }

    impl Fake {
        fn commit(&self, id: &str, files: &[(&str, &str)]) {
            let mut state = self.state.lock().unwrap();
            let files = files
                .iter()
                .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()));
            state.commits.insert(id.to_owned(), files.collect());
            id.clone_into(&mut state.head);
        }

        fn change(&self, changed: &[(&str, Option<&str>)]) {
            let changed = changed
                .iter()
                .map(|(path, text)| ((*path).to_owned(), text.map(str::to_owned)));
            self.state.lock().unwrap().changed = changed.collect();
        }

        fn blobs_read(&self) -> Vec<String> {
            std::mem::take(&mut self.state.lock().unwrap().blobs_read)
        }
    }

    impl Source for Fake {
        fn head(&self) -> Result<String, Fault> {
            Ok(self.state.lock().unwrap().head.clone())
        }

        fn tree(&self, commit: &str) -> Result<Vec<(String, String)>, Fault> {
            let state = self.state.lock().unwrap();
            let files = state.commits.get(commit).cloned().unwrap_or_default();
            Ok(files
                .into_iter()
                .map(|(path, text)| (path, blob(&text)))
                .collect())
        }

        fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault> {
            let mut state = self.state.lock().unwrap();
            state.blobs_read.extend(blobs.iter().cloned());
            let texts: BTreeMap<String, String> = state
                .commits
                .values()
                .flat_map(|files| files.values().map(|text| (blob(text), text.clone())))
                .collect();
            Ok(blobs
                .iter()
                .filter_map(|id| Some((id.clone(), texts.get(id)?.clone())))
                .collect())
        }

        fn changed(&self, _: &Path, _: &str) -> Result<Vec<(String, Option<String>)>, Fault> {
            Ok(self.state.lock().unwrap().changed.clone())
        }
    }

    const MANIFEST: &str = "[package]\nname = \"a\"\n";

    async fn repo(dir: &TempDir) -> (RepoMap<Fake>, Fake) {
        let db = Db::open(dir).await.unwrap();
        let workspace = db.workspace(&WorkspaceId::try_from("one".to_owned()).unwrap());
        let map = workspace.map(&RepoId::try_from("repo".to_owned()).unwrap());
        let source = Fake::default();
        (
            RepoMap {
                map,
                source: source.clone(),
            },
            source,
        )
    }

    fn items(facts: &[Facts], path: &str) -> String {
        weacon_codemap::assemble(facts).item(path, 1_000)
    }

    #[tokio::test]
    async fn reads_only_blobs_it_has_not_seen() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        let lib = "mod m;\npub fn run() { m::go(); }\n";
        source.commit(
            "c1",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", lib),
                ("src/m.rs", "pub fn go() {}\n"),
                ("web/app.ts", "x"),
            ],
        );
        let (commit, facts) = repo.base().await.unwrap();
        assert_eq!((commit.as_str(), facts.len()), ("c1", 3));
        assert_eq!(source.blobs_read().len(), 3);
        assert!(items(&facts, "a::run").contains("calls fn a::m::go"));

        repo.base().await.unwrap();
        assert_eq!(source.blobs_read(), Vec::<String>::new());

        let moved = "pub fn went() {}\n";
        source.commit(
            "c2",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", lib),
                ("src/m.rs", moved),
            ],
        );
        let (_, facts) = repo.base().await.unwrap();
        assert_eq!(source.blobs_read(), [blob(moved)]);
        assert!(items(&facts, "a::m::went").starts_with("fn a::m::went"));
    }

    #[tokio::test]
    async fn a_worktree_sees_its_changes_over_the_base() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        let lib = "mod m;\npub fn run() { m::go(); }\n";
        source.commit(
            "c1",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", lib),
                ("src/m.rs", "pub fn go() {}\n"),
                ("README.md", "# Old\n"),
            ],
        );
        source.change(&[
            ("src/m.rs", Some("pub fn go() {}\npub fn stop() {}\n")),
            ("README.md", None),
            ("src/new.rs", Some("fn x() {}")),
        ]);
        let worktree = Path::new("/w/chat");
        let facts = repo.worktree(worktree).await.unwrap();
        let paths: BTreeSet<&str> = facts.iter().map(Facts::path).collect();
        assert_eq!(
            paths,
            BTreeSet::from(["Cargo.toml", "src/lib.rs", "src/m.rs", "src/new.rs"])
        );
        assert!(items(&facts, "a::m::stop").starts_with("fn a::m::stop"));

        let layer = layer_name(worktree);
        let found = repo.map.search(&layer, "stop", 10).await.unwrap();
        assert_eq!(
            found
                .iter()
                .map(|hit| hit.name.as_str())
                .collect::<Vec<_>>(),
            ["a::m::stop"]
        );
        assert_eq!(repo.map.search(&layer, "old", 10).await.unwrap(), []);
        assert_eq!(repo.map.search(BASE, "old", 10).await.unwrap().len(), 1);
        assert_eq!(repo.map.search(&layer, "run", 10).await.unwrap().len(), 1);
    }

    #[test]
    fn maps_manifests_sources_and_docs() {
        let mapped = ["Cargo.toml", "a/Cargo.toml", "a/src/lib.rs", "docs/x.md"];
        assert!(mapped.iter().all(|path| is_mapped(path)));
        let skipped = ["a/tests/it.rs", "a/build.rs", "web/app.ts", "Cargo.lock"];
        assert!(!skipped.iter().any(|path| is_mapped(path)));
        assert!(content_key("a").starts_with("sha256:ca978112"));
    }
}
