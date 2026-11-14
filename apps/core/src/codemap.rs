//! The code map of each repository a workspace registered: a base layer at the main worktree's
//! commit, and a layer per worktree with what it changed, kept in the core's database. It
//! never leaves this machine.

#[cfg(test)]
mod fake;
mod repo;
mod watch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use weacon_codemap::{Facts, Reader, SourceFile};
use weacon_observability::Fault;
use weacon_protocol::{Sha256 as Sha256Hex, code};

use crate::app::blocking;
use crate::db::{BASE, Layer, Map, Stored};

/// The most hits a search returns before its answer is cut to its budget.
const SEARCHED: u32 = 50;

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

/// The base layer's commit and the facts of its files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Base {
    pub(crate) commit: String,
    pub(crate) facts: Vec<Facts>,
}

/// One repository's code map, read from `source` and kept in `map`.
#[derive(Clone, Debug)]
pub(crate) struct RepoMap<S> {
    pub(crate) map: Map,
    pub(crate) source: S,
}

impl<S: Source> RepoMap<S> {
    /// The commit the main worktree is at.
    pub(crate) async fn head(&self) -> Result<String, Fault> {
        self.blocking(Source::head).await
    }

    /// The facts of every file at the main worktree's commit, reading only files whose blob
    /// the map hasn't seen.
    pub(crate) async fn base(&self) -> Result<Base, Fault> {
        let commit = self.head().await?;
        if let Some(layer) = self.map.layer(BASE).await?
            && layer.commit == commit
        {
            let facts = self.stored_facts(&layer.files).await?;
            return Ok(Base { commit, facts });
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
        Ok(Base { commit, facts })
    }

    /// What the worktree at `path` changed from `base`'s commit.
    pub(crate) async fn changed(
        &self,
        base: &Base,
        path: &Path,
    ) -> Result<Vec<(String, Option<String>)>, Fault> {
        let (path, commit) = (path.to_owned(), base.commit.clone());
        let changed = self
            .blocking(move |source| source.changed(&path, &commit))
            .await?;
        Ok(changed
            .into_iter()
            .filter(|(path, _)| is_mapped(path))
            .collect())
    }

    /// The facts of every file as the worktree at `path` has it: `base`'s, with what the
    /// worktree `changed` in their place. Only text the map hasn't seen is read.
    pub(crate) async fn worktree(
        &self,
        base: &Base,
        path: &Path,
        changed: Vec<(String, Option<String>)>,
    ) -> Result<Vec<Facts>, Fault> {
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
            .facts
            .iter()
            .filter(|facts| !shadowed.contains(facts.path()))
            .cloned()
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

    /// What search finds for `words` in the worktree at `path`, or in the base without one,
    /// best first, within `budget` bytes.
    pub(crate) async fn search(
        &self,
        path: Option<&Path>,
        words: &str,
        budget: usize,
    ) -> Result<String, Fault> {
        let layer = path.map_or_else(|| BASE.to_owned(), layer_name);
        let hits = self.map.search(&layer, words, SEARCHED).await?;
        let header = format!("Found {}:", hits.len());
        let lines = hits
            .into_iter()
            .map(|hit| format!("- {} {}  {}", hit.kind, hit.name, hit.path));
        Ok(weacon_codemap::fit(
            std::iter::once(header).chain(lines),
            budget,
        ))
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
        blocking(move || Ok(call(&source))).await.map_err(|error| {
            Fault::new(code!("map.read_failed"), "The repository couldn't be read").because(error)
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
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str());
    name == "Cargo.toml" || extension == Some("md") || (in_src && extension == Some("rs"))
}

/// What a worktree's text is stored under: its hash, which no git blob id looks like.
fn content_key(text: &str) -> String {
    let digest = Sha256Hex::from_bytes(Sha256::digest(text.as_bytes()).into());
    format!("sha256:{}", digest.as_str())
}

/// The layer that holds what the worktree at `path` changed.
fn layer_name(path: &Path) -> String {
    format!("worktree:{}", PathBuf::from(path).display())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{RepoId, WorkspaceId};
    use weacon_testing::TempDir;

    use super::fake::{Fake, blob};
    use super::*;
    use crate::db::Db;

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
        let Base { commit, facts } = repo.base().await.unwrap();
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
        let facts = repo.base().await.unwrap().facts;
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
        let base = repo.base().await.unwrap();
        let changed = repo.changed(&base, worktree).await.unwrap();
        let facts = repo.worktree(&base, worktree, changed).await.unwrap();
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

    #[tokio::test]
    async fn searches_a_worktree_or_the_base_within_a_budget() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        source.commit(
            "c1",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", "pub fn serve() {}\npub fn served() {}\n"),
            ],
        );
        source.change(&[("docs/serving.md", Some("# Serving pages\n"))]);
        let base = repo.base().await.unwrap();
        let worktree = Path::new("/w/chat");
        let changed = repo.changed(&base, worktree).await.unwrap();
        repo.worktree(&base, worktree, changed).await.unwrap();

        let found = repo.search(Some(worktree), "serv", 10_000).await.unwrap();
        let mut lines: Vec<&str> = found.lines().collect();
        assert_eq!(lines.remove(0), "Found 3:");
        lines.sort_unstable();
        assert_eq!(
            lines,
            [
                "- doc docs/serving.md  docs/serving.md",
                "- fn a::serve  src/lib.rs",
                "- fn a::served  src/lib.rs",
            ]
        );
        assert_eq!(
            repo.search(None, "serv", 10_000)
                .await
                .unwrap()
                .lines()
                .count(),
            3
        );
        assert_eq!(
            repo.search(None, "serv", 40).await.unwrap(),
            "Found 2:\n… 2 more lines\n"
        );
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
