use std::collections::BTreeMap;

use sqlx::{QueryBuilder, Row, Sqlite};
use weacon_codemap::Facts;
use weacon_observability::Fault;
use weacon_protocol::RepoId;

use super::{Workspace, failed, unreadable};

/// The base layer: the repository at a commit. Every other layer is one worktree's changes.
pub(crate) const BASE: &str = "base";

/// How many rows one insert carries, well within `SQLite`'s limit on bound values.
const BATCH: usize = 100;

/// One repository's code map within a workspace.
#[derive(Clone, Debug)]
pub(crate) struct Map {
    workspace: Workspace,
    repo: RepoId,
}

/// A layer as it is stored: its name, the commit it was read at, and its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layer {
    pub(crate) name: String,
    pub(crate) commit: String,
    pub(crate) files: Vec<Stored>,
}

/// A file of a layer, by the git blob its text has; `None` in a worktree's layer for a file
/// the worktree deleted.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Stored {
    pub(crate) path: String,
    pub(crate) blob: Option<String>,
}

/// Something search can find: an item by its path, or a document by its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) name: String,
}

impl Workspace {
    pub(crate) fn map(&self, repo: &RepoId) -> Map {
        Map {
            workspace: self.clone(),
            repo: repo.clone(),
        }
    }
}

impl Map {
    fn keys(&self) -> (&str, &str) {
        (self.workspace.id.as_str(), self.repo.as_str())
    }

    /// The layer named `name`, if it has been stored.
    pub(crate) async fn layer(&self, name: &str) -> Result<Option<Layer>, Fault> {
        let (workspace, repo) = self.keys();
        let pool = &self.workspace.db.pool;
        let commit: Option<String> = sqlx::query_scalar(
            "SELECT commit_id FROM map_layers WHERE workspace_id = ? AND repo = ? AND layer = ?",
        )
        .bind(workspace)
        .bind(repo)
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(failed)?;
        let Some(commit) = commit else {
            return Ok(None);
        };
        let rows = sqlx::query(
            "SELECT path, blob FROM map_files WHERE workspace_id = ? AND repo = ? AND layer = ? \
             ORDER BY path",
        )
        .bind(workspace)
        .bind(repo)
        .bind(name)
        .fetch_all(pool)
        .await
        .map_err(failed)?;
        let files = rows
            .iter()
            .map(|row| {
                Ok(Stored {
                    path: row.try_get("path").map_err(failed)?,
                    blob: row.try_get("blob").map_err(failed)?,
                })
            })
            .collect::<Result<_, Fault>>()?;
        Ok(Some(Layer {
            name: name.to_owned(),
            commit,
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
            let rows = query
                .build()
                .fetch_all(&self.workspace.db.pool)
                .await
                .map_err(failed)?;
            for row in rows {
                let path: String = row.try_get("path").map_err(failed)?;
                let blob: String = row.try_get("blob").map_err(failed)?;
                let json: String = row.try_get("facts").map_err(failed)?;
                let facts = serde_json::from_str(&json).map_err(|_| unreadable())?;
                found.insert((path, blob), facts);
            }
        }
        Ok(found)
    }

    /// Stores `layer` in place of what was there, with the facts of files new to this repo's
    /// map and what search finds in the layer.
    pub(crate) async fn replace(
        &self,
        layer: &Layer,
        facts: &[(String, String, Facts)],
        entries: &[Entry],
    ) -> Result<(), Fault> {
        let (workspace, repo) = self.keys();
        let mut transaction = self.workspace.db.pool.begin().await.map_err(failed)?;
        sqlx::query("INSERT OR IGNORE INTO map_repos (workspace_id, repo) VALUES (?, ?)")
            .bind(workspace)
            .bind(repo)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
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
            "INSERT INTO map_layers (workspace_id, repo, layer, commit_id) VALUES (?, ?, ?, ?)",
        )
        .bind(workspace)
        .bind(repo)
        .bind(&layer.name)
        .bind(&layer.commit)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        for batch in layer.files.chunks(BATCH) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT INTO map_files (workspace_id, repo, layer, path, blob) ",
            );
            query.push_values(batch, |mut row, file| {
                row.push_bind(workspace)
                    .push_bind(repo)
                    .push_bind(&layer.name)
                    .push_bind(&file.path)
                    .push_bind(&file.blob);
            });
            query
                .build()
                .execute(&mut *transaction)
                .await
                .map_err(failed)?;
        }
        for batch in facts.chunks(BATCH) {
            let rows = batch
                .iter()
                .map(|(path, blob, facts)| {
                    let json = serde_json::to_string(facts).map_err(|_| unreadable())?;
                    Ok((path, blob, json))
                })
                .collect::<Result<Vec<_>, Fault>>()?;
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT OR REPLACE INTO map_facts (workspace_id, repo, path, blob, facts) ",
            );
            query.push_values(rows, |mut row, (path, blob, json)| {
                row.push_bind(workspace)
                    .push_bind(repo)
                    .push_bind(path)
                    .push_bind(blob)
                    .push_bind(json);
            });
            query
                .build()
                .execute(&mut *transaction)
                .await
                .map_err(failed)?;
        }
        for batch in entries.chunks(BATCH) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT INTO map_search (workspace_id, repo, layer, path, kind, name, body) ",
            );
            query.push_values(batch, |mut row, entry| {
                row.push_bind(workspace)
                    .push_bind(repo)
                    .push_bind(&layer.name)
                    .push_bind(&entry.path)
                    .push_bind(&entry.kind)
                    .push_bind(&entry.name)
                    .push_bind(&entry.body);
            });
            query
                .build()
                .execute(&mut *transaction)
                .await
                .map_err(failed)?;
        }
        transaction.commit().await.map_err(failed)
    }

    /// What matches every word of `words` in `layer`, best first: a worktree's layer finds its
    /// own files and the base's files it didn't change.
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
        let rows = sqlx::query(
            "SELECT path, kind, name FROM map_search \
             WHERE map_search MATCH ?1 AND workspace_id = ?2 AND repo = ?3 \
             AND (layer = ?4 OR (layer = 'base' AND path NOT IN (SELECT path FROM map_files \
                 WHERE workspace_id = ?2 AND repo = ?3 AND layer = ?4))) \
             ORDER BY rank LIMIT ?5",
        )
        .bind(query)
        .bind(workspace)
        .bind(repo)
        .bind(layer)
        .bind(limit)
        .fetch_all(&self.workspace.db.pool)
        .await
        .map_err(failed)?;
        rows.iter()
            .map(|row| {
                Ok(Hit {
                    path: row.try_get("path").map_err(failed)?,
                    kind: row.try_get("kind").map_err(failed)?,
                    name: row.try_get("name").map_err(failed)?,
                })
            })
            .collect()
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
            .map(&RepoId::try_from("repo".to_owned()).unwrap())
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
            files,
        }
    }

    fn entry(path: &str, name: &str, body: &str) -> Entry {
        Entry {
            path: path.to_owned(),
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

    #[test]
    fn quotes_every_word_of_a_search() {
        assert_eq!(
            match_query(" a  \"b "),
            Some("\"a\"* \"\"\"b\"*".to_owned())
        );
        assert_eq!(match_query(""), None);
    }
}
