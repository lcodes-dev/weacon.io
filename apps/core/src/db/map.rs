mod write;

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Sqlite};
use weacon_codemap::{Entry, Facts};
use weacon_observability::Fault;
use weacon_protocol::Sha256 as Sha256Hex;

use super::{Workspace, failed};
use write::{
    BATCH, add_repo, drop_unused_facts, insert_entries, insert_facts, insert_files, newer_than,
};

/// The base layer: the repository at a commit. Every other layer is one worktree's changes.
pub(crate) const BASE: &str = "base";

/// A repository on this machine, known by its main worktree's canonical path; its map is kept
/// under a hash of that path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalRepo(String);

impl LocalRepo {
    pub(crate) fn at(main_worktree: &Path) -> Self {
        let digest = Sha256::digest(main_worktree.as_os_str().as_encoded_bytes());
        Self(Sha256Hex::from_bytes(digest.into()).as_str().to_owned())
    }
}

/// One repository's code map within a workspace.
#[derive(Clone, Debug)]
pub(crate) struct Map {
    workspace: Workspace,
    repo: LocalRepo,
}

/// A layer as it is stored: its name, the base commit it was read at, the refresh that read
/// it, and its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layer {
    pub(crate) name: String,
    pub(crate) commit: String,
    pub(crate) generation: i64,
    pub(crate) files: Vec<Stored>,
}

/// A file of a layer, by the git blob its text has; `None` in a worktree's layer for a file
/// the worktree deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored {
    pub(crate) path: String,
    pub(crate) blob: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) name: String,
}

impl Workspace {
    pub(crate) fn map(&self, repo: &LocalRepo) -> Map {
        Map {
            workspace: self.clone(),
            repo: repo.clone(),
        }
    }
}

impl Map {
    fn keys(&self) -> (&str, &str) {
        (self.workspace.id.as_str(), self.repo.0.as_str())
    }

    /// A number for a refresh about to start, above every earlier one's, so the layer it reads
    /// never replaces one a later refresh stored.
    pub(crate) async fn begin(&self) -> Result<i64, Fault> {
        let (workspace, repo) = self.keys();
        let mut transaction = self.workspace.db.pool.begin().await.map_err(failed)?;
        add_repo(&mut transaction, (workspace, repo)).await?;
        let generation = sqlx::query_scalar(
            "UPDATE map_repos SET generation = generation + 1 \
             WHERE workspace_id = ? AND repo = ? RETURNING generation",
        )
        .bind(workspace)
        .bind(repo)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)?;
        Ok(generation)
    }

    /// The layer named `name`, if it has been stored.
    pub(crate) async fn layer(&self, name: &str) -> Result<Option<Layer>, Fault> {
        let (workspace, repo) = self.keys();
        let mut transaction = self.workspace.db.pool.begin().await.map_err(failed)?;
        let stored: Option<(String, i64)> = sqlx::query_as(
            "SELECT commit_id, generation FROM map_layers \
             WHERE workspace_id = ? AND repo = ? AND layer = ?",
        )
        .bind(workspace)
        .bind(repo)
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let Some((commit, generation)) = stored else {
            return Ok(None);
        };
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT path, blob FROM map_files WHERE workspace_id = ? AND repo = ? AND layer = ? \
             ORDER BY path",
        )
        .bind(workspace)
        .bind(repo)
        .bind(name)
        .fetch_all(&mut *transaction)
        .await
        .map_err(failed)?;
        let files = rows
            .into_iter()
            .map(|(path, blob)| Stored { path, blob })
            .collect();
        Ok(Some(Layer {
            name: name.to_owned(),
            commit,
            generation,
            files,
        }))
    }

    /// The facts stored for each `(path, blob)`, by that pair; pairs without any are left out.
    pub(crate) async fn facts(
        &self,
        wanted: &[(String, String)],
    ) -> Result<BTreeMap<(String, String), Facts>, Fault> {
        let (workspace, repo) = self.keys();
        let mut found = BTreeMap::new();
        for batch in wanted.chunks(BATCH) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT path, blob, facts FROM map_facts WHERE workspace_id = ",
            );
            query
                .push_bind(workspace)
                .push(" AND repo = ")
                .push_bind(repo);
            query.push(" AND (path, blob) IN (");
            let mut pairs = query.separated(", ");
            for (path, blob) in batch {
                pairs.push("(").push_bind_unseparated(path);
                pairs
                    .push_unseparated(", ")
                    .push_bind_unseparated(blob)
                    .push_unseparated(")");
            }
            query.push(")");
            let rows: Vec<(String, String, String)> = query
                .build_query_as()
                .fetch_all(&self.workspace.db.pool)
                .await
                .map_err(failed)?;
            // A row this build can't read counts as missing, so the file is read again.
            found.extend(rows.into_iter().filter_map(|(path, blob, json)| {
                Some(((path, blob), serde_json::from_str(&json).ok()?))
            }));
        }
        Ok(found)
    }

    /// Stores `layer` in place of what was there, with the facts of files new to this repo's
    /// map and what search finds in the layer; nothing when a later refresh stored it already.
    pub(crate) async fn replace(
        &self,
        layer: &Layer,
        facts: &[(String, String, Facts)],
        entries: &[Entry],
    ) -> Result<(), Fault> {
        let (workspace, repo) = self.keys();
        let mut transaction = self.workspace.db.pool.begin().await.map_err(failed)?;
        add_repo(&mut transaction, (workspace, repo)).await?;
        if newer_than(
            &mut transaction,
            (workspace, repo, &layer.name),
            layer.generation,
        )
        .await?
        {
            return Ok(());
        }
        for statement in [
            "DELETE FROM map_layers WHERE workspace_id = ? AND repo = ? AND layer = ?",
            "DELETE FROM map_search WHERE workspace_id = ? AND repo = ? AND layer = ?",
        ] {
            sqlx::query(statement)
                .bind(workspace)
                .bind(repo)
                .bind(&layer.name)
                .execute(&mut *transaction)
                .await
                .map_err(failed)?;
        }
        sqlx::query(
            "INSERT INTO map_layers (workspace_id, repo, layer, commit_id, generation) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(workspace)
        .bind(repo)
        .bind(&layer.name)
        .bind(&layer.commit)
        .bind(layer.generation)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        let keys = (workspace, repo, layer.name.as_str());
        insert_files(&mut transaction, keys, &layer.files).await?;
        insert_facts(&mut transaction, (workspace, repo), facts).await?;
        insert_entries(&mut transaction, keys, entries).await?;
        drop_unused_facts(&mut transaction, (workspace, repo)).await?;
        transaction.commit().await.map_err(failed)
    }

    /// What matches every word of `words` in `layer`, best first: a worktree's layer finds its
    /// own files and the base's files it didn't change, and nothing once the base has moved on
    /// from the commit it was read at.
    pub(crate) async fn search(
        &self,
        layer: &str,
        words: &str,
        limit: u32,
    ) -> Result<Vec<Hit>, Fault> {
        let Some(query) = match_query(words) else {
            return Ok(Vec::new());
        };
        let (workspace, repo) = self.keys();
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT path, kind, name FROM map_search \
             WHERE map_search MATCH ?1 AND workspace_id = ?2 AND repo = ?3 \
             AND (layer = ?4 OR (layer = ?6 AND path NOT IN (SELECT path FROM map_files \
                 WHERE workspace_id = ?2 AND repo = ?3 AND layer = ?4))) \
             AND EXISTS (SELECT 1 FROM map_layers AS overlay JOIN map_layers AS base \
                 ON base.workspace_id = overlay.workspace_id AND base.repo = overlay.repo \
                 AND base.layer = ?6 AND base.commit_id = overlay.commit_id \
                 WHERE overlay.workspace_id = ?2 AND overlay.repo = ?3 AND overlay.layer = ?4) \
             ORDER BY rank LIMIT ?5",
        )
        .bind(query)
        .bind(workspace)
        .bind(repo)
        .bind(layer)
        .bind(limit)
        .bind(BASE)
        .fetch_all(&self.workspace.db.pool)
        .await
        .map_err(failed)?;
        Ok(rows
            .into_iter()
            .map(|(path, kind, name)| Hit { path, kind, name })
            .collect())
    }

    /// Makes every stored facts row one this build can't read.
    #[cfg(test)]
    pub(crate) async fn spoil_facts(&self) {
        sqlx::query("UPDATE map_facts SET facts = '{'")
            .execute(&self.workspace.db.pool)
            .await
            .unwrap();
    }

    /// Removes this repository's map, every layer of it.
    pub(crate) async fn forget(&self) -> Result<(), Fault> {
        let (workspace, repo) = self.keys();
        let mut transaction = self.workspace.db.pool.begin().await.map_err(failed)?;
        for statement in [
            "DELETE FROM map_search WHERE workspace_id = ? AND repo = ?",
            "DELETE FROM map_repos WHERE workspace_id = ? AND repo = ?",
        ] {
            sqlx::query(statement)
                .bind(workspace)
                .bind(repo)
                .execute(&mut *transaction)
                .await
                .map_err(failed)?;
        }
        transaction.commit().await.map_err(failed)
    }
}

/// Each word as a quoted FTS5 prefix term, so no input reads as query syntax; `None` without
/// words.
fn match_query(words: &str) -> Option<String> {
    let terms: Vec<String> = words
        .split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use weacon_codemap::{Reader, SourceFile};
    use weacon_protocol::WorkspaceId;
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;

    async fn map(dir: &TempDir, workspace: &str) -> Map {
        let db = Db::open(dir).await.unwrap();
        let workspace = WorkspaceId::try_from(workspace.to_owned()).unwrap();
        db.workspace(&workspace)
            .map(&LocalRepo::at(Path::new("/repo")))
    }

    fn stored(path: &str, blob: Option<&str>) -> Stored {
        Stored {
            path: path.to_owned(),
            blob: blob.map(str::to_owned),
        }
    }

    fn layer(name: &str, files: Vec<Stored>) -> Layer {
        Layer {
            name: name.to_owned(),
            commit: "c0ffee".to_owned(),
            generation: 0,
            files,
        }
    }

    fn entry(path: &str, name: &str, body: &str) -> Entry {
        Entry {
            file: path.to_owned(),
            kind: "fn".to_owned(),
            name: name.to_owned(),
            body: body.to_owned(),
        }
    }

    fn facts(path: &str, text: &str) -> Facts {
        let file = SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        };
        Reader::new().unwrap().facts(&file).unwrap().unwrap()
    }

    #[tokio::test]
    async fn stores_and_replaces_layers_with_their_facts() {
        let dir = TempDir::new().unwrap();
        let map = map(&dir, "one").await;
        assert_eq!(map.layer(BASE).await.unwrap(), None);
        let lib = facts("src/lib.rs", "fn run() {}\n");
        let base = layer(BASE, vec![stored("src/lib.rs", Some("b1"))]);
        let new = [("src/lib.rs".to_owned(), "b1".to_owned(), lib.clone())];
        map.replace(&base, &new, &[]).await.unwrap();
        assert_eq!(map.layer(BASE).await.unwrap(), Some(base));

        let wanted = [
            ("src/lib.rs".to_owned(), "b1".to_owned()),
            ("src/lib.rs".to_owned(), "b2".to_owned()),
        ];
        let found = map.facts(&wanted).await.unwrap();
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            [(wanted[0].clone(), lib)]
        );

        let moved = layer(BASE, vec![stored("src/other.rs", Some("b3"))]);
        map.replace(&moved, &[], &[]).await.unwrap();
        assert_eq!(map.layer(BASE).await.unwrap(), Some(moved));
    }

    #[tokio::test]
    async fn a_worktree_finds_its_own_files_and_the_base_files_it_kept() {
        let dir = TempDir::new().unwrap();
        let map = map(&dir, "one").await;
        let base = layer(
            BASE,
            vec![stored("a.rs", Some("1")), stored("b.rs", Some("2"))],
        );
        let entries = [
            entry("a.rs", "a::serve", "serves pages"),
            entry("b.rs", "a::render", "renders pages"),
        ];
        map.replace(&base, &[], &entries).await.unwrap();
        let worktree = layer(
            "wt:chat",
            vec![stored("a.rs", None), stored("c.rs", Some("3"))],
        );
        map.replace(
            &worktree,
            &[],
            &[entry("c.rs", "a::paint", "paints \"pages\"")],
        )
        .await
        .unwrap();

        let names = |hits: Vec<Hit>| hits.into_iter().map(|hit| hit.name).collect::<Vec<_>>();
        let mut found = names(map.search("wt:chat", "pag", 10).await.unwrap());
        found.sort();
        assert_eq!(found, ["a::paint", "a::render"]);
        let mut found = names(map.search(BASE, "pages", 10).await.unwrap());
        found.sort();
        assert_eq!(found, ["a::render", "a::serve"]);
        assert_eq!(
            names(map.search(BASE, "serves pages", 10).await.unwrap()),
            ["a::serve"]
        );
        assert_eq!(
            names(map.search(BASE, "\" OR body:*", 10).await.unwrap()),
            Vec::<String>::new()
        );
        assert_eq!(
            names(map.search(BASE, "  ", 10).await.unwrap()),
            Vec::<String>::new()
        );
        assert_eq!(map.search(BASE, "pages", 1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn workspaces_never_see_each_others_maps() {
        let dir = TempDir::new().unwrap();
        let (one, two) = (map(&dir, "one").await, map(&dir, "two").await);
        let lib = facts("src/lib.rs", "fn run() {}\n");
        let mine = layer("wt:mine", vec![stored("src/lib.rs", Some("b1"))]);
        let new = [("src/lib.rs".to_owned(), "b1".to_owned(), lib)];
        one.replace(&mine, &new, &[entry("src/lib.rs", "a::run", "secret")])
            .await
            .unwrap();
        one.replace(&layer(BASE, vec![]), &[], &[]).await.unwrap();
        let theirs = layer("wt:theirs", vec![stored("src/x.rs", Some("b9"))]);
        two.replace(&theirs, &[], &[]).await.unwrap();

        assert_eq!(two.layer("wt:mine").await.unwrap(), None);
        assert_eq!(one.layer("wt:theirs").await.unwrap(), None);
        let wanted = [("src/lib.rs".to_owned(), "b1".to_owned())];
        assert!(two.facts(&wanted).await.unwrap().is_empty());
        assert!(
            two.search("wt:mine", "secret", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(one.search("wt:mine", "secret", 10).await.unwrap().len(), 1);

        two.forget().await.unwrap();
        assert!(one.layer("wt:mine").await.unwrap().is_some());
        one.forget().await.unwrap();
        assert_eq!(one.layer("wt:mine").await.unwrap(), None);
        assert!(one.facts(&wanted).await.unwrap().is_empty());
        assert!(
            one.search("wt:mine", "secret", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn each_repository_has_its_own_map() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(&dir).await.unwrap();
        let workspace = db.workspace(&WorkspaceId::try_from("one".to_owned()).unwrap());
        let (one, two) = (Path::new("/one"), Path::new("/two"));
        assert_eq!(LocalRepo::at(one), LocalRepo::at(one));
        let mine = layer(BASE, vec![stored("src/lib.rs", Some("b1"))]);
        workspace
            .map(&LocalRepo::at(one))
            .replace(&mine, &[], &[])
            .await
            .unwrap();
        assert_eq!(
            workspace
                .map(&LocalRepo::at(two))
                .layer(BASE)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            workspace
                .map(&LocalRepo::at(one))
                .layer(BASE)
                .await
                .unwrap(),
            Some(mine)
        );
    }

    #[tokio::test]
    async fn rereads_what_it_cant_parse_and_drops_what_nothing_uses() {
        let dir = TempDir::new().unwrap();
        let map = map(&dir, "one").await;
        let lib = facts("src/lib.rs", "fn run() {}\n");
        let first = layer(BASE, vec![stored("src/lib.rs", Some("b1"))]);
        let new = [("src/lib.rs".to_owned(), "b1".to_owned(), lib.clone())];
        map.replace(&first, &new, &[]).await.unwrap();
        let one = [("src/lib.rs".to_owned(), "b1".to_owned())];
        assert_eq!(map.facts(&one).await.unwrap().len(), 1);

        sqlx::query("UPDATE map_facts SET facts = '{'")
            .execute(&map.workspace.db.pool)
            .await
            .unwrap();
        assert!(map.facts(&one).await.unwrap().is_empty());

        let second = layer(BASE, vec![stored("src/lib.rs", Some("b2"))]);
        let new = [("src/lib.rs".to_owned(), "b2".to_owned(), lib)];
        map.replace(&second, &new, &[]).await.unwrap();
        let two = [("src/lib.rs".to_owned(), "b2".to_owned())];
        assert_eq!(map.facts(&two).await.unwrap().len(), 1);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM map_facts")
            .fetch_one(&map.workspace.db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn an_earlier_refresh_never_replaces_a_later_ones_layer() {
        let dir = TempDir::new().unwrap();
        let map = map(&dir, "one").await;
        let (earlier, later) = (map.begin().await.unwrap(), map.begin().await.unwrap());
        assert!(later > earlier);
        let read = |commit: &str, generation| Layer {
            commit: commit.to_owned(),
            generation,
            ..layer(BASE, vec![])
        };
        map.replace(&read("b", later), &[], &[]).await.unwrap();
        map.replace(&read("a", earlier), &[], &[]).await.unwrap();
        let stored = map.layer(BASE).await.unwrap().unwrap();
        assert_eq!((stored.commit.as_str(), stored.generation), ("b", later));
        map.replace(&read("c", later), &[], &[]).await.unwrap();
        assert_eq!(map.layer(BASE).await.unwrap().unwrap().commit, "c");
    }

    #[tokio::test]
    async fn a_worktree_finds_nothing_once_the_base_moved_on() {
        let dir = TempDir::new().unwrap();
        let map = map(&dir, "one").await;
        let at = |name: &str, commit: &str, files| Layer {
            commit: commit.to_owned(),
            ..layer(name, files)
        };
        map.replace(
            &at(BASE, "a", vec![]),
            &[],
            &[entry("src/lib.rs", "a::run", "x")],
        )
        .await
        .unwrap();
        let worktree = at("wt:mine", "a", vec![stored("src/new.rs", Some("k"))]);
        map.replace(&worktree, &[], &[entry("src/new.rs", "a::runner", "y")])
            .await
            .unwrap();
        assert_eq!(map.search("wt:mine", "run", 10).await.unwrap().len(), 2);

        map.replace(
            &at(BASE, "b", vec![]),
            &[],
            &[entry("src/lib.rs", "a::run", "x")],
        )
        .await
        .unwrap();
        assert!(map.search("wt:mine", "run", 10).await.unwrap().is_empty());
        assert_eq!(map.search(BASE, "run", 10).await.unwrap().len(), 1);
    }

    #[test]
    fn quotes_every_word_of_a_search() {
        assert_eq!(
            match_query(" a  \"b "),
            Some("\"a\"* \"\"\"b\"*".to_owned())
        );
        assert_eq!(match_query(""), None);
    }
}
