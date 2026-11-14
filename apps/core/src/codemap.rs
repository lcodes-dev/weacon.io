//! The code map of each repository a workspace registered: a base layer at the main worktree's
//! commit, and a layer per worktree with what it changed, kept in the core's database. It
//! never leaves this machine.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the daemon and the MCP tools start reading the map in their own changes"
    )
)]

#[cfg(test)]
mod fake;
mod repo;
mod watch;

pub(crate) use repo::LARGEST;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};
use weacon_codemap::{Entry, Facts, Reader, SourceFile, is_mapped};
use weacon_observability::Fault;
use weacon_protocol::{CommitId, Sha256 as Sha256Hex, code};

use crate::app::blocking;
use crate::db::{BASE, Layer, Map, Stored};

/// The most hits a search returns before its answer is cut to its budget.
const SEARCHED: u32 = 50;

/// What the code map reads from a repository. Every call may block.
pub(crate) trait Source: Clone + Send + Sync + 'static {
    /// The commit the main worktree is at.
    fn head(&self) -> Result<CommitId, Fault>;
    /// Each file at `commit`, with its blob.
    fn tree(&self, commit: &CommitId) -> Result<Vec<(String, String)>, Fault>;
    /// The text of each blob that is UTF-8, by blob.
    fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault>;
    /// Each file that differs in the worktree at `path` from `commit`, committed or not.
    fn changed(&self, path: &Path, commit: &CommitId) -> Result<Vec<String>, Fault>;
    /// The text of `file` in the worktree at `path`; none once deleted, or when the map can't
    /// take it.
    fn text(&self, path: &Path, file: &str) -> Option<String>;
}

/// The base layer's commit and the facts of its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Base {
    pub(crate) commit: CommitId,
    pub(crate) files: Vec<Stored>,
    pub(crate) facts: Vec<Facts>,
    /// What search finds in the base.
    pub(crate) entries: BTreeSet<Entry>,
}

/// What a worktree changed, each file with its text now, `None` once deleted or when it can't
/// be read, and the refresh that read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Changes {
    pub(crate) generation: i64,
    pub(crate) files: Vec<(String, Option<String>)>,
}

/// One repository's code map, read from `source` and kept in `map`.
#[derive(Clone, Debug)]
pub(crate) struct RepoMap<S> {
    pub(crate) map: Map,
    pub(crate) source: S,
}

impl<S: Source> RepoMap<S> {
    /// The commit the main worktree is at.
    pub(crate) async fn head(&self) -> Result<CommitId, Fault> {
        self.blocking(Source::head).await
    }

    /// The facts of every file at the main worktree's commit, reading only files whose blob
    /// the map hasn't seen or can't read back.
    pub(crate) async fn base(&self) -> Result<Base, Fault> {
        let generation = self.map.begin().await?;
        let commit = self.head().await?;
        if let Some(layer) = self.map.layer(BASE).await?
            && layer.commit == commit.as_str()
            && let (facts, missing) = self.cached(&layer.files).await?
            && missing.is_empty()
        {
            self.map.touch(BASE, generation).await?;
            let (facts, entries) = off_runtime(move || {
                let entries = entries_of(&facts);
                Ok((facts, entries))
            })
            .await?;
            return Ok(Base {
                commit,
                files: layer.files,
                facts,
                entries,
            });
        }
        self.read_base(commit, generation).await
    }

    /// The base at `commit`, reading the blobs the map hasn't seen, then stored as refresh
    /// `generation` read it.
    async fn read_base(&self, commit: CommitId, generation: i64) -> Result<Base, Fault> {
        let tree = {
            let commit = commit.clone();
            self.blocking(move |source| source.tree(&commit)).await?
        };
        let files: Vec<Stored> = tree
            .into_iter()
            .filter(|(path, _)| is_mapped(path))
            .map(|(path, blob)| Stored {
                path,
                blob: Some(blob),
            })
            .collect();
        let (facts, missing) = self.cached(&files).await?;
        let blobs: Vec<String> = missing.iter().map(|(_, blob)| blob.clone()).collect();
        let texts = self.blocking(move |source| source.blobs(&blobs)).await?;
        let (read, facts, entries) = off_runtime(move || {
            let read = read(missing.into_iter().map(|(path, blob)| {
                let text = texts.get(&blob).cloned();
                (path, blob, text)
            }))?;
            let mut facts = facts;
            facts.extend(read.iter().filter_map(|(_, _, facts)| facts.clone()));
            let entries = entries_of(&facts);
            Ok((read, facts, entries))
        })
        .await?;
        let layer = Layer {
            name: BASE.to_owned(),
            commit: commit.as_str().to_owned(),
            generation,
            files,
        };
        let stored: Vec<Entry> = entries.iter().cloned().collect();
        self.map.replace(&layer, &read, &stored).await?;
        Ok(Base {
            commit,
            files: layer.files,
            facts,
            entries,
        })
    }

    /// What the worktree at `path` changed from `base`'s commit, as a refresh numbered before
    /// it read anything.
    pub(crate) async fn changed(&self, base: &Base, path: &Path) -> Result<Changes, Fault> {
        let generation = self.map.begin().await?;
        let (path, commit) = (path.to_owned(), base.commit.clone());
        let files = self
            .blocking(move |source| {
                let changed = source.changed(&path, &commit)?;
                Ok(changed
                    .into_iter()
                    .filter(|file| is_mapped(file))
                    .map(|file| {
                        let text = source.text(&path, &file);
                        (file, text)
                    })
                    .collect())
            })
            .await?;
        Ok(Changes { generation, files })
    }

    /// The facts of every file as the worktree at `path` has it: `base`'s, with what the
    /// worktree `changed` in their place. Only text the map hasn't seen is read.
    pub(crate) async fn worktree(
        &self,
        base: &Base,
        path: &Path,
        changed: Changes,
    ) -> Result<Vec<Facts>, Fault> {
        let Changes { generation, files } = changed;
        let keyed: Vec<(String, Option<(String, String)>)> = files
            .into_iter()
            .map(|(path, text)| (path, text.map(|text| (content_key(&text), text))))
            .collect();
        let changed: Vec<Stored> = keyed
            .iter()
            .map(|(path, text)| Stored {
                path: path.clone(),
                blob: text.as_ref().map(|(key, _)| key.clone()),
            })
            .collect();
        let (overlay, missing) = self.cached(&changed).await?;
        let commit = base.commit.as_str().to_owned();
        let base = base.clone();
        let (read, facts, files, entries) = off_runtime(move || {
            let missing: BTreeSet<(String, String)> = missing.into_iter().collect();
            let read = read(keyed.into_iter().filter_map(|(path, text)| {
                let (key, text) = text?;
                missing
                    .contains(&(path.clone(), key.clone()))
                    .then_some((path, key, Some(text)))
            }))?;
            let shadowed: BTreeSet<&str> = changed.iter().map(|file| file.path.as_str()).collect();
            let mut facts: Vec<Facts> = base
                .facts
                .iter()
                .filter(|facts| !shadowed.contains(facts.path()))
                .cloned()
                .collect();
            facts.extend(overlay);
            facts.extend(read.iter().filter_map(|(_, _, facts)| facts.clone()));
            let (files, entries) = overlay_of(&base, changed, &facts);
            Ok((read, facts, files, entries))
        })
        .await?;
        let layer = Layer {
            name: layer_name(path),
            commit,
            generation,
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

    /// The stored facts of `files`, and the files whose text hasn't been read yet.
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
                Some(stored) => facts.extend(stored),
                None => missing.push(key),
            }
        }
        Ok((facts, missing))
    }

    async fn blocking<T: Send + 'static>(
        &self,
        call: impl FnOnce(&S) -> Result<T, Fault> + Send + 'static,
    ) -> Result<T, Fault> {
        let source = self.source.clone();
        blocking(move || Ok(call(&source)))
            .await
            .map_err(|error| read_failed().because(error))?
    }
}

/// A worktree's layer over `base`: the files it `changed`, and those whose search entries its
/// changes moved, since a change can rename what other files hold, such as their crate; and
/// what search finds in them.
fn overlay_of(base: &Base, changed: Vec<Stored>, facts: &[Facts]) -> (Vec<Stored>, Vec<Entry>) {
    let entries = entries_of(facts);
    let moved: BTreeSet<String> = entries
        .symmetric_difference(&base.entries)
        .map(|entry| entry.file.clone())
        .chain(changed.iter().map(|file| file.path.clone()))
        .collect();
    let changed_paths: BTreeSet<String> = changed.iter().map(|file| file.path.clone()).collect();
    let mut files = changed;
    files.extend(
        base.files
            .iter()
            .filter(|file| moved.contains(&file.path) && !changed_paths.contains(&file.path))
            .cloned(),
    );
    let entries = entries
        .into_iter()
        .filter(|entry| moved.contains(&entry.file))
        .collect();
    (files, entries)
}

/// What search finds in a repository with `facts`.
fn entries_of(facts: &[Facts]) -> BTreeSet<Entry> {
    weacon_codemap::assemble(facts)
        .entries()
        .into_iter()
        .collect()
}

/// Runs `work`, which is CPU-bound, off the async runtime.
pub(crate) async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
) -> Result<T, Fault> {
    blocking(move || Ok(work()))
        .await
        .map_err(|error| index_failed().because(error))?
}

/// The facts of each `(path, key, text)`, with the path and key they're stored under; no text,
/// or text that adds nothing such as a broken manifest, gives none, which is kept too.
fn read(
    texts: impl Iterator<Item = (String, String, Option<String>)>,
) -> Result<Vec<(String, String, Option<Facts>)>, Fault> {
    let mut reader = Reader::new().map_err(|error| index_failed().because(error))?;
    let mut found = Vec::new();
    for (path, key, text) in texts {
        let facts = match text {
            Some(text) => {
                let file = SourceFile {
                    path: path.clone(),
                    text,
                };
                reader
                    .facts(&file)
                    .map_err(|error| index_failed().because(error))?
            }
            None => None,
        };
        found.push((path, key, facts));
    }
    Ok(found)
}

pub(crate) fn read_failed() -> Fault {
    Fault::new(code!("map.read_failed"), "The repository couldn't be read")
}

pub(crate) fn index_failed() -> Fault {
    Fault::new(code!("map.index_failed"), "The code map couldn't be built")
}

/// What a worktree's text is stored under: its hash, which no git blob id looks like.
fn content_key(text: &str) -> String {
    let digest = Sha256Hex::from_bytes(Sha256::digest(text.as_bytes()).into());
    format!("sha256:{}", digest.as_str())
}

/// The layer that holds what the worktree at `path` changed.
fn layer_name(path: &Path) -> String {
    format!("worktree:{}", path.display())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::WorkspaceId;
    use weacon_testing::TempDir;

    use super::fake::{Fake, blob};
    use super::*;
    use crate::db::Db;

    const MANIFEST: &str = "[package]\nname = \"a\"\n";

    async fn repo(dir: &TempDir) -> (RepoMap<Fake>, Fake) {
        let db = Db::open(dir).await.unwrap();
        let workspace = db.workspace(&WorkspaceId::try_from("one".to_owned()).unwrap());
        let map = workspace.map(&crate::db::LocalRepo::at(Path::new("/repo")));
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
        let Base { commit, facts, .. } = repo.base().await.unwrap();
        assert_eq!((commit, facts.len()), (fake::commit_id("c1"), 3));
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
    async fn a_stored_base_reads_again_what_it_cant_read_back() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        source.commit(
            "c1",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", "pub fn run() {}\n"),
            ],
        );
        assert_eq!(repo.base().await.unwrap().facts.len(), 2);
        assert_eq!(source.blobs_read().len(), 2);
        assert_eq!(repo.base().await.unwrap().facts.len(), 2);
        assert!(source.blobs_read().is_empty());

        repo.map.spoil_facts().await;
        assert_eq!(repo.base().await.unwrap().facts.len(), 2);
        assert_eq!(source.blobs_read().len(), 2);
        assert_eq!(repo.base().await.unwrap().facts.len(), 2);
        assert!(source.blobs_read().is_empty());
    }

    #[tokio::test]
    async fn text_with_no_facts_is_read_once() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        source.commit("c1", &[("Cargo.toml", "[package"), ("README.md", "# A\n")]);
        assert_eq!(repo.base().await.unwrap().facts.len(), 1);
        assert_eq!(source.blobs_read().len(), 2);
        assert_eq!(repo.base().await.unwrap().facts.len(), 1);
        assert!(source.blobs_read().is_empty());
    }

    #[tokio::test]
    async fn a_refresh_that_started_earlier_stores_nothing_over_a_later_one() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        source.commit("c1", &[("Cargo.toml", MANIFEST)]);
        let base = repo.base().await.unwrap();
        let earlier = repo.map.begin().await.unwrap();
        repo.base().await.unwrap();
        let stale = Layer {
            name: BASE.to_owned(),
            commit: "other".to_owned(),
            generation: earlier,
            files: Vec::new(),
        };
        repo.map.replace(&stale, &[], &[]).await.unwrap();
        assert_eq!(
            repo.map.layer(BASE).await.unwrap().unwrap().commit,
            base.commit.as_str()
        );

        let worktree = Path::new("/w/chat");
        source.change(&[("src/old.rs", Some("fn old() {}"))]);
        let older = repo.changed(&base, worktree).await.unwrap();
        source.change(&[("src/new.rs", Some("fn new() {}"))]);
        let newer = repo.changed(&base, worktree).await.unwrap();
        repo.worktree(&base, worktree, newer).await.unwrap();
        repo.worktree(&base, worktree, older).await.unwrap();
        let layer = repo
            .map
            .layer(&layer_name(worktree))
            .await
            .unwrap()
            .unwrap();
        let paths: Vec<&str> = layer.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["src/new.rs"]);
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
            ("dump.sql", Some("huge")),
        ]);
        let worktree = Path::new("/w/chat");
        let base = repo.base().await.unwrap();
        let changed = repo.changed(&base, worktree).await.unwrap();
        assert_eq!(source.texts_read(), ["src/m.rs", "README.md", "src/new.rs"]);
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
    async fn a_worktree_finds_what_its_changes_renamed_in_other_files() {
        let dir = TempDir::new().unwrap();
        let (repo, source) = repo(&dir).await;
        source.commit(
            "c1",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", "pub fn run() {}\n"),
            ],
        );
        source.change(&[("Cargo.toml", Some("[package]\nname = \"b\"\n"))]);
        let base = repo.base().await.unwrap();
        let worktree = Path::new("/w/chat");
        let changed = repo.changed(&base, worktree).await.unwrap();
        repo.worktree(&base, worktree, changed).await.unwrap();

        let found = repo.search(Some(worktree), "run", 10_000).await.unwrap();
        assert_eq!(found, "Found 1:\n- fn b::run  src/lib.rs\n");
        let found = repo.search(None, "run", 10_000).await.unwrap();
        assert_eq!(found, "Found 1:\n- fn a::run  src/lib.rs\n");
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
    fn keys_text_by_its_hash_and_layers_by_their_worktree() {
        assert!(content_key("a").starts_with("sha256:ca978112"));
        assert_eq!(layer_name(Path::new("/w/chat")), "worktree:/w/chat");
    }
}
