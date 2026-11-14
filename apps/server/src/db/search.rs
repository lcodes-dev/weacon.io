use sqlx::SqliteConnection;
use weacon_model::{Doc, Task};
use weacon_observability::Fault;
use weacon_protocol::{Mark, SearchHit, Snippet};

use super::{Workspace, failed, stored, unreadable};
use crate::search::query::Query;

/// The bytes a snippet marks a match with; indexed text holds neither.
const MATCH_START: char = '\u{1}';
const MATCH_END: char = '\u{2}';

/// What search finds a task or doc by, and when it last changed.
struct Indexed<'a> {
    kind: &'static str,
    item: &'a str,
    title: &'a str,
    body: &'a str,
    at: i64,
}

type HitRow = (String, String, String, String);

impl Workspace {
    /// Indexes `task` by its title, as changed at `at`.
    pub(super) async fn index_task(
        &self,
        transaction: &mut SqliteConnection,
        task: &Task,
        at: i64,
    ) -> Result<(), Fault> {
        let indexed = Indexed {
            kind: "task",
            item: task.id.as_str(),
            title: task.title.as_str(),
            body: "",
            at,
        };
        self.index(transaction, indexed).await
    }

    /// Indexes `doc` by its title, and its body followed by one line per pin, as revised at
    /// `at`.
    pub(super) async fn index_doc(
        &self,
        transaction: &mut SqliteConnection,
        doc: &Doc,
        at: i64,
    ) -> Result<(), Fault> {
        let pins = doc
            .text
            .anchors()
            .into_iter()
            .flat_map(|anchors| &anchors.pins);
        let body = pins.fold(doc.text.body().to_owned(), |body, pin| {
            body + "\n" + pin.as_str()
        });
        let indexed = Indexed {
            kind: "doc",
            item: doc.id.as_str(),
            title: doc.text.title().as_str(),
            body: &body,
            at,
        };
        self.index(transaction, indexed).await
    }

    /// Puts `indexed` in the search index in place of what it held for the same item.
    async fn index(
        &self,
        transaction: &mut SqliteConnection,
        indexed: Indexed<'_>,
    ) -> Result<(), Fault> {
        let Indexed {
            kind,
            item,
            title,
            body,
            at,
        } = indexed;
        sqlx::query(
            "DELETE FROM search WHERE rowid =
                 (SELECT row FROM search_rows WHERE workspace_id = ? AND kind = ? AND item = ?)",
        )
        .bind(self.id.as_str())
        .bind(kind)
        .bind(item)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        let row = sqlx::query(
            "INSERT INTO search (workspace_id, kind, item, title, body) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(kind)
        .bind(item)
        .bind(unmarked(title))
        .bind(unmarked(body))
        .execute(&mut *transaction)
        .await
        .map_err(failed)?
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO search_rows (workspace_id, kind, item, row, at) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (workspace_id, kind, item) DO UPDATE SET row = excluded.row, at = excluded.at",
        )
        .bind(self.id.as_str())
        .bind(kind)
        .bind(item)
        .bind(row)
        .bind(at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// The workspace's tasks and docs that match `query`, at most `limit` of them: those
    /// matching in their title first, then the most recently changed.
    pub(crate) async fn search(&self, query: &Query, limit: u32) -> Result<Vec<SearchHit>, Fault> {
        let short = serde_json::to_string(&query.short).map_err(|_| unreadable())?;
        sqlx::query_as::<_, HitRow>(
            "SELECT search.kind, search.item, search.title,
                 snippet(search, -1, char(1), char(2), '…', 64)
             FROM search
             JOIN search_rows AS rows
                 ON rows.workspace_id = search.workspace_id AND rows.row = search.rowid
             WHERE search MATCH ?1 AND search.workspace_id = ?2
                 AND NOT EXISTS (SELECT 1 FROM json_each(?3)
                     WHERE search.title || char(10) || search.body NOT LIKE value ESCAPE '\\')
             ORDER BY search.rowid IN
                     (SELECT rowid FROM search
                      WHERE search MATCH 'title : (' || ?1 || ')' AND workspace_id = ?2) DESC,
                 rows.at DESC, search.rowid DESC
             LIMIT ?4",
        )
        .bind(&query.words)
        .bind(self.id.as_str())
        .bind(short)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(hit)
        .collect()
    }
}

fn hit((kind, item, title, marked): HitRow) -> Result<SearchHit, Fault> {
    let snippet = snippet(&marked);
    match kind.as_str() {
        "task" => Ok(SearchHit::Task {
            task: stored(item)?,
            title,
            snippet,
        }),
        "doc" => Ok(SearchHit::Doc {
            doc: stored(item)?,
            title,
            snippet,
        }),
        _ => Err(unreadable()),
    }
}

/// `text` without the bytes snippets mark matches with, which the schema's backfill also
/// leaves out.
fn unmarked(text: &str) -> String {
    text.replace([MATCH_START, MATCH_END], " ")
}

/// A snippet's text and marks, from one with its matches between the marking bytes.
fn snippet(marked: &str) -> Snippet {
    let mut snippet = Snippet::default();
    let mut start = None;
    for ch in marked.chars() {
        match ch {
            MATCH_START => start = Some(utf16_len(&snippet.text)),
            MATCH_END => {
                if let Some(start) = start.take() {
                    let end = utf16_len(&snippet.text);
                    snippet.marks.push(Mark { start, end });
                }
            }
            ch => snippet.text.push(ch),
        }
    }
    snippet
}

fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use weacon_model::{Attribution, Doc, DocText, Revision, TaskEdit, Title};
    use weacon_protocol::{CommitId, DocAnchors, DocId, DocPin, TaskId};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;
    use crate::db::schema::database_before;
    use crate::search::query::parse;
    use crate::test_support::{email, fresh_account, member_id, new_task, two_teams, workspace_id};

    fn by(at: i64) -> Attribution {
        Attribution::by(member_id("m1"), at)
    }

    // In w2, ada is m2.
    fn in_two(at: i64) -> Attribution {
        Attribution::by(member_id("m2"), at)
    }

    fn doc(id: &str, title: &str, body: &str, pinned: &[&str]) -> Doc {
        let anchors = (!pinned.is_empty()).then(|| DocAnchors {
            commit: CommitId::try_from("c".repeat(40)).unwrap(),
            pins: pinned
                .iter()
                .map(|anchor| DocPin::try_from(format!("{anchor}#0123456789abcdef")).unwrap())
                .collect(),
        });
        let title = Title::try_from(title.to_owned()).unwrap();
        let text = DocText::new(title, body.to_owned(), anchors).unwrap();
        Doc::new(DocId::try_from(id.to_owned()).unwrap(), text)
    }

    async fn found(workspace: &Workspace, text: &str) -> Vec<String> {
        let query = parse(text).unwrap().unwrap();
        let hits = workspace.search(&query, 50).await.unwrap();
        hits.into_iter()
            .map(|hit| match hit {
                SearchHit::Task { task, .. } => task.as_str().to_owned(),
                SearchHit::Doc { doc, .. } => doc.as_str().to_owned(),
                SearchHit::Newer => "newer".to_owned(),
            })
            .collect()
    }

    #[tokio::test]
    async fn title_matches_come_first_then_the_most_recently_changed() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.add_task(&new_task("t1", "Release notes", "a"), &by(10))
            .await
            .unwrap();
        let body = "Steps for the release.";
        one.add_doc(&doc("d1", "Plan", body, &[]), &by(30))
            .await
            .unwrap();
        one.add_task(&new_task("t2", "Ship the release", "b"), &by(20))
            .await
            .unwrap();

        assert_eq!(found(&one, "release").await, ["t2", "t1", "d1"]);
        assert_eq!(found(&one, "release steps").await, ["d1"]);
        let query = parse("release").unwrap().unwrap();
        let first = one.search(&query, 1).await.unwrap();
        let SearchHit::Task { task, title, .. } = &first[0] else {
            panic!("{first:?}");
        };
        assert_eq!((task.as_str(), title.as_str()), ("t2", "Ship the release"));
    }

    #[tokio::test]
    async fn a_search_never_finds_another_workspaces_rows() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        one.add_task(&new_task("t1", "Release", "a"), &by(1))
            .await
            .unwrap();
        // The same ids and text in w2, changed later, so they would rank first.
        two.add_task(&new_task("t1", "Release", "a"), &in_two(2))
            .await
            .unwrap();
        two.add_doc(&doc("d1", "Release", "release", &[]), &in_two(3))
            .await
            .unwrap();
        let query = parse("release").unwrap().unwrap();

        assert_eq!(found(&one, "release").await, ["t1"]);
        assert_eq!(one.search(&query, 1).await.unwrap().len(), 1);
        assert_eq!(found(&two, "release").await, ["d1", "t1"]);
        // Reindexing w2's t1 leaves w1's alone.
        let task = TaskId::try_from("t1".to_owned()).unwrap();
        let retitled = TaskEdit::Title(Title::try_from("Shipped".to_owned()).unwrap());
        two.edit_task(&task, Revision::FIRST, retitled, &in_two(4))
            .await
            .unwrap();
        assert_eq!(found(&one, "release").await, ["t1"]);
        assert!(found(&one, "shipped").await.is_empty());
        assert_eq!(found(&two, "shipped").await, ["t1"]);
    }

    #[tokio::test]
    async fn a_change_is_found_by_its_new_text_only() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Draft", "a");
        one.add_task(&task, &by(1)).await.unwrap();
        let retitled = TaskEdit::Title(Title::try_from("Polish".to_owned()).unwrap());
        one.edit_task(&task.id, Revision::FIRST, retitled, &by(2))
            .await
            .unwrap();
        let first = doc(
            "d1",
            "Framing",
            "Lines end in [x](sym:frame::encode).",
            &["sym:frame::encode"],
        );
        one.add_doc(&first, &by(3)).await.unwrap();
        let revised = doc("d1", "Framing", "Whole frames.", &[]).text;
        one.revise_doc(&first.id, Revision::FIRST, revised, &by(4))
            .await
            .unwrap();

        assert!(found(&one, "draft").await.is_empty());
        assert_eq!(found(&one, "polish").await, ["t1"]);
        assert!(found(&one, "lines").await.is_empty());
        assert!(found(&one, "frame::encode").await.is_empty());
        assert_eq!(found(&one, "whole").await, ["d1"]);
    }

    #[tokio::test]
    async fn pins_are_found_and_accents_and_case_are_not_told_apart() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let pinned = doc(
            "d1",
            "Café",
            "See [x](sym:frame::encode).",
            &["sym:frame::encode"],
        );
        one.add_doc(&pinned, &by(1)).await.unwrap();

        assert_eq!(found(&one, "encode_line").await, Vec::<String>::new());
        assert_eq!(found(&one, "#0123456789abcdef").await, ["d1"]);
        assert_eq!(found(&one, "CAFE").await, ["d1"]);
        assert_eq!(found(&one, "café").await, ["d1"]);
    }

    #[tokio::test]
    async fn query_syntax_in_the_text_is_only_ever_text() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let odd = new_task("t1", "say \"NEAR(x* OR title: {a} ^b", "a");
        one.add_task(&odd, &by(1)).await.unwrap();

        for text in ["\"NEAR(x*", "title:", "{a}", "OR \"NEAR(x*", "say ^b"] {
            assert_eq!(found(&one, text).await, ["t1"], "{text:?}");
        }
        for text in ["NEAR(say)", "say AND", "-say", "say*z"] {
            assert!(found(&one, text).await.is_empty(), "{text:?}");
        }
    }

    #[tokio::test]
    async fn short_words_narrow_what_long_ones_find_as_plain_text() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        for (id, title) in [
            ("t1", "Launch Q4 at 5%"),
            ("t2", "Launch party"),
            ("t3", "Launch a_b"),
        ] {
            one.add_task(&new_task(id, title, id), &by(1))
                .await
                .unwrap();
        }

        assert_eq!(found(&one, "launch q4").await, ["t1"]);
        assert_eq!(found(&one, "launch 5%").await, ["t1"]);
        assert_eq!(found(&one, "launch %").await, ["t1"]);
        assert_eq!(found(&one, "launch a_").await, ["t3"]);
        assert!(found(&one, "launch _x").await.is_empty());
    }

    #[test]
    fn a_snippet_marks_its_matches_in_utf16_units() {
        let marked = "😀 \u{1}ab\u{2}é \u{1}c\u{2}\u{2}d";

        assert_eq!(
            snippet(marked),
            Snippet {
                text: "😀 abé cd".to_owned(),
                marks: vec![Mark { start: 3, end: 5 }, Mark { start: 7, end: 8 }],
            }
        );
        assert_eq!(
            snippet("plain"),
            Snippet {
                text: "plain".to_owned(),
                marks: vec![]
            }
        );
    }

    #[tokio::test]
    async fn a_snippet_shows_the_whole_match_and_text_around_it() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let body = format!(
            "{} internationalization_of_everything {}",
            "x ".repeat(40),
            "y ".repeat(40)
        );
        one.add_doc(&doc("d1", "Notes", &body, &[]), &by(1))
            .await
            .unwrap();
        let query = parse("internationalization_of_everything")
            .unwrap()
            .unwrap();

        let hits = one.search(&query, 1).await.unwrap();
        let SearchHit::Doc { snippet, .. } = &hits[0] else {
            panic!("{hits:?}");
        };
        let word = "internationalization_of_everything";
        let [Mark { start, end }] = snippet.marks[..] else {
            panic!("{snippet:?}");
        };
        let units: Vec<u16> = snippet.text.encode_utf16().collect();
        let range = usize::try_from(start).unwrap()..usize::try_from(end).unwrap();
        assert_eq!(String::from_utf16(&units[range]).unwrap(), word);
        assert!(snippet.text.starts_with("… x x"), "{snippet:?}");
        assert!(snippet.text.ends_with("y y…"), "{snippet:?}");
    }

    #[tokio::test]
    async fn text_holding_the_marking_bytes_is_indexed_without_them() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let marked = doc("d1", "Notes", "a\u{1}release\u{2}b", &[]);
        one.add_doc(&marked, &by(1)).await.unwrap();
        let query = parse("release").unwrap().unwrap();

        let hits = one.search(&query, 1).await.unwrap();
        let SearchHit::Doc { snippet, .. } = &hits[0] else {
            panic!("{hits:?}");
        };
        assert_eq!(snippet.text, "a release b");
        assert_eq!(snippet.marks, [Mark { start: 2, end: 9 }]);
    }

    async fn index_rows(db: &Db) -> Vec<(String, String, String, String, String, i64)> {
        sqlx::query_as(
            "SELECT search.workspace_id, search.kind, search.item, title, body, at
             FROM search JOIN search_rows
                 ON search_rows.workspace_id = search.workspace_id AND row = search.rowid
             ORDER BY search.kind, search.item",
        )
        .fetch_all(&db.readers)
        .await
        .unwrap()
    }

    // A task and a pinned doc as they were stored before search.
    fn stored_before() -> String {
        format!(
            "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO tasks VALUES ('w1', 't1', 'Ship it', 'todo', 'a', 2);
             INSERT INTO task_versions (workspace_id, task_id, version, author_kind,
                 author_member, accepted_by, at)
                 VALUES ('w1', 't1', 1, 'person', 'm1', 'm1', 5),
                        ('w1', 't1', 2, 'person', 'm1', 'm1', 6);
             INSERT INTO docs VALUES ('w1', 'd1', 1);
             INSERT INTO doc_versions (workspace_id, doc_id, version, title, body, commit_id,
                 author_kind, author_member, accepted_by, at)
                 VALUES ('w1', 'd1', 1, 'Framing', 'See{marks}[a](sym:a) and [b](area:b).',
                     '{commit}', 'person', 'm1', 'm1', 7);
             INSERT INTO doc_pins VALUES
                 ('w1', 'd1', 1, 1, 'area:b#0123456789abcdef'),
                 ('w1', 'd1', 1, 0, 'sym:a#0123456789abcdef');",
            marks = "\u{1}\u{2}",
            commit = "c".repeat(40),
        )
    }

    #[tokio::test]
    async fn what_was_stored_before_search_is_indexed_as_a_write_indexes_it() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("old.db");
        database_before(&path, "CREATE VIRTUAL TABLE search", &stored_before()).await;
        let old = Db::open(&path).await.unwrap();
        let new = Db::create(&dir.join("new.db")).await.unwrap();
        let one = new
            .create_workspace(&workspace_id("w1"), "One")
            .await
            .unwrap();
        let ada = email("ada@example.com");
        one.add_member(&member_id("m1"), &fresh_account(), &ada)
            .await
            .unwrap();
        let task = new_task("t1", "Ship it", "a");
        one.add_task(&task, &by(5)).await.unwrap();
        let done = TaskEdit::Status(weacon_protocol::TaskStatus::Done);
        one.edit_task(&task.id, Revision::FIRST, done, &by(6))
            .await
            .unwrap();
        let pinned = doc(
            "d1",
            "Framing",
            "See\u{1}\u{2}[a](sym:a) and [b](area:b).",
            &["sym:a", "area:b"],
        );
        one.add_doc(&pinned, &by(7)).await.unwrap();

        let rows = index_rows(&old).await;
        assert_eq!(rows, index_rows(&new).await);
        assert_eq!(rows.len(), 2);
    }
}
