mod snippet;

use sqlx::SqliteConnection;
use weacon_model::{Doc, Task};
use weacon_observability::Fault;
use weacon_protocol::{ChatEvent, ChatId, RunEvent, RunUpdate, SearchHit};

use super::chats::Row;
use super::{Workspace, failed, stored, unreadable};
use crate::search::query::{Marks, Query};

/// Control bytes indexed text leaves out, as the schema's backfill does.
const MATCH_START: char = '\u{1}';
const MATCH_END: char = '\u{2}';

// What a search row is of; a message's item is its chat and its place in the log, `c-1/12`.
const TASK: &str = "task";
const DOC: &str = "doc";
const CHAT: &str = "chat";
const MESSAGE: &str = "message";

/// What search finds a task, doc, chat or message by, and when it last changed.
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
            kind: TASK,
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
            kind: DOC,
            item: doc.id.as_str(),
            title: doc.text.title().as_str(),
            body: &body,
            at,
        };
        self.index(transaction, indexed).await
    }

    /// Indexes the chat `chat` by its title, as made at `at`.
    pub(super) async fn index_chat(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        title: &str,
        at: i64,
    ) -> Result<(), Fault> {
        let indexed = Indexed {
            kind: CHAT,
            item: chat.as_str(),
            title,
            body: "",
            at,
        };
        self.index(transaction, indexed).await
    }

    /// Indexes each of `rows`, stored in `chat`'s log, that is a message: a person's prompt or
    /// an agent's text.
    pub(super) async fn index_messages(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        rows: &[Row<'_>],
    ) -> Result<(), Fault> {
        for row in rows {
            let Some(text) = message_text(row.event) else {
                continue;
            };
            let item = message_item(chat, row.seq);
            let indexed = Indexed {
                kind: MESSAGE,
                item: &item,
                title: "",
                body: text,
                at: row.at,
            };
            self.index(transaction, indexed).await?;
        }
        Ok(())
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

    /// The workspace's tasks, docs, chats and messages that match `query`, at most `limit` of
    /// them: those matching in their title first, then the most recently changed. A message
    /// takes its chat's title.
    pub(crate) async fn search(&self, query: &Query, limit: u32) -> Result<Vec<SearchHit>, Fault> {
        let short = serde_json::to_string(&query.short).map_err(|_| unreadable())?;
        sqlx::query_as::<_, HitRow>(
            "SELECT search.kind, search.item,
                 COALESCE((SELECT title FROM chats
                     WHERE chats.workspace_id = search.workspace_id
                         AND chats.chat_id = substr(search.item, 1, instr(search.item, '/') - 1)
                         AND search.kind = 'message'), search.title),
                 search.body
             FROM search
             JOIN search_rows AS rows
                 ON rows.workspace_id = search.workspace_id AND rows.row = search.rowid
             WHERE search MATCH ?1 AND search.workspace_id = ?2
                 AND NOT EXISTS (SELECT 1 FROM json_each(?3)
                     WHERE search.title || char(10) || search.body NOT LIKE value ESCAPE '\\')
             ORDER BY search.rowid IN
                     (SELECT rowid FROM search
                      WHERE search MATCH 'title : (' || ?1 || ')' AND workspace_id = ?2)
                 AND NOT EXISTS (SELECT 1 FROM json_each(?3)
                     WHERE search.title NOT LIKE value ESCAPE '\\') DESC,
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
        .map(|row| hit(row, &query.marks))
        .collect()
    }
}

fn hit((kind, item, title, body): HitRow, marks: &Marks) -> Result<SearchHit, Fault> {
    let snippet = snippet::of(&body, &title, marks);
    match kind.as_str() {
        TASK => Ok(SearchHit::Task {
            task: stored(item)?,
            title,
            snippet,
        }),
        DOC => Ok(SearchHit::Doc {
            doc: stored(item)?,
            title,
            snippet,
        }),
        CHAT => Ok(SearchHit::Chat {
            chat: stored(item)?,
            title,
            seq: None,
            snippet,
        }),
        MESSAGE => {
            let (chat, seq) = item.split_once('/').ok_or_else(unreadable)?;
            Ok(SearchHit::Chat {
                chat: stored(chat.to_owned())?,
                title,
                seq: Some(seq.parse().map_err(|_| unreadable())?),
                snippet,
            })
        }
        _ => Err(unreadable()),
    }
}

/// How the search index names the event at `seq` in `chat`'s log.
fn message_item(chat: &ChatId, seq: i64) -> String {
    format!("{}/{seq}", chat.as_str())
}

/// The message `event` carries, if it carries one.
fn message_text(event: &ChatEvent) -> Option<&str> {
    match event {
        ChatEvent::Prompt { text, .. }
        | ChatEvent::OutsidePrompt { text }
        | ChatEvent::Run {
            update:
                RunUpdate {
                    event: RunEvent::Text { text },
                    ..
                },
        } => Some(text),
        ChatEvent::Run { .. }
        | ChatEvent::Receipt { .. }
        | ChatEvent::RunStatus { .. }
        | ChatEvent::Newer => None,
    }
}

/// `text` without the bytes snippets mark matches with, which the schema's backfill also
/// leaves out.
fn unmarked(text: &str) -> String {
    text.replace([MATCH_START, MATCH_END], " ")
}

#[cfg(test)]
mod tests {
    use weacon_model::{Attribution, Doc, DocText, Revision, TaskEdit, Title};
    use weacon_protocol::{CommitId, DocAnchors, DocId, DocPin, Mark, TaskId};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;
    use crate::db::schema::{database_before, steps};
    use crate::db::test_support::{append, make_titled, said};
    use crate::search::query::{MAX_WORD, parse};
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
            repo: None,
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
                SearchHit::Chat { chat, seq, .. } => match seq {
                    Some(seq) => format!("{}/{seq}", chat.as_str()),
                    None => chat.as_str().to_owned(),
                },
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
    async fn a_title_holding_the_short_words_too_comes_first() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.add_task(&new_task("t1", "Release v2", "a"), &by(5))
            .await
            .unwrap();
        one.add_doc(&doc("d1", "Release", "Notes for v2.", &[]), &by(40))
            .await
            .unwrap();

        assert_eq!(found(&one, "release v2").await, ["t1", "d1"]);
        assert_eq!(found(&one, "release").await, ["d1", "t1"]);
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

    async fn marked(workspace: &Workspace, word: &str) -> (String, String) {
        let query = parse(word).unwrap().unwrap();
        let hits = workspace.search(&query, 1).await.unwrap();
        let SearchHit::Doc { snippet, .. } = &hits[0] else {
            panic!("{hits:?}");
        };
        let [Mark { start, end }] = snippet.marks[..] else {
            panic!("{snippet:?}");
        };
        let units: Vec<u16> = snippet.text.encode_utf16().collect();
        let range = usize::try_from(start).unwrap()..usize::try_from(end).unwrap();
        let marked = String::from_utf16(&units[range]).unwrap();
        (marked, snippet.text.clone())
    }

    #[tokio::test]
    async fn a_snippet_shows_the_whole_match_and_text_around_it() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let longest = "a".repeat(MAX_WORD);
        let body = format!("{}release {longest} {}", "x ".repeat(40), "y ".repeat(40));
        one.add_doc(&doc("d1", "Notes", &body, &[]), &by(1))
            .await
            .unwrap();

        let (word, around) = marked(&one, "release").await;
        assert_eq!(word, "release");
        assert!(around.starts_with("… x x"), "{around:?}");
        let (word, _) = marked(&one, &longest).await;
        assert_eq!(word, longest);
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
        database_before(&path, steps::SEARCH, &stored_before()).await;
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

    // c-1 titled "Fix the build" in w1, whose log holds a prompt, an agent's text, a text
    // being written and an outside prompt, at seqs 1 to 4.
    async fn chatted(workspace: &Workspace) {
        make_titled(workspace, "c-1", "m1", "Fix the build", 3)
            .await
            .unwrap();
        let prompt = ChatEvent::Prompt {
            pick: None,
            request: "r-2".parse().unwrap(),
            text: "Why is the deploy flaky".to_owned(),
            repo: None,
            agent: None,
        };
        let delta = ChatEvent::Run {
            update: RunUpdate {
                run: "run-1".parse().unwrap(),
                event: RunEvent::TextDelta {
                    text: "Half written".to_owned(),
                },
            },
        };
        let outside = ChatEvent::OutsidePrompt {
            text: "Typed in the terminal".to_owned(),
        };
        let events = [
            (Some(1), prompt),
            (Some(2), said("The cache expired")),
            (Some(3), delta),
            (Some(4), outside),
        ];
        append(workspace, "c-1", &events).await.unwrap();
    }

    #[tokio::test]
    async fn a_chat_is_found_by_its_title_and_the_messages_its_log_keeps() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        chatted(&one).await;

        assert_eq!(found(&one, "build").await, ["c-1"]);
        assert_eq!(found(&one, "flaky").await, ["c-1/1"]);
        assert_eq!(found(&one, "cache").await, ["c-1/2"]);
        assert_eq!(found(&one, "terminal").await, ["c-1/4"]);
        assert!(found(&one, "written").await.is_empty());
        assert!(found(&two, "build").await.is_empty());
        let query = parse("cache").unwrap().unwrap();
        let hits = one.search(&query, 5).await.unwrap();
        let [SearchHit::Chat { title, .. }] = hits.as_slice() else {
            panic!("{hits:?}");
        };
        assert_eq!(title, "Fix the build");
    }

    #[tokio::test]
    async fn a_message_takes_the_title_of_its_own_workspaces_chat() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        for (workspace, owner, title) in
            [(&one, "m1", "Fix the build"), (&two, "m2", "Plan the trip")]
        {
            make_titled(workspace, "c-1", owner, title, 3)
                .await
                .unwrap();
            append(workspace, "c-1", &[(Some(1), said("The cache expired"))])
                .await
                .unwrap();
        }

        let query = parse("cache").unwrap().unwrap();
        for (workspace, title) in [(&one, "Fix the build"), (&two, "Plan the trip")] {
            let hits = workspace.search(&query, 5).await.unwrap();
            assert!(
                matches!(hits.as_slice(), [SearchHit::Chat { title: found, seq: Some(1), .. }] if found == title),
                "{hits:?}"
            );
        }
    }

    #[tokio::test]
    async fn chats_stored_before_their_search_are_indexed_as_a_write_indexes_them() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        chatted(&one).await;
        let written = index_rows(&one.db).await;
        sqlx::raw_sql(
            "DELETE FROM search WHERE kind IN ('chat', 'message');
             DELETE FROM search_rows WHERE kind IN ('chat', 'message');",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();

        sqlx::raw_sql(steps::SEARCH_CHATS)
            .execute(&one.db.writer)
            .await
            .unwrap();
        assert_eq!(index_rows(&one.db).await, written);
        assert_eq!(written.len(), 4);
    }

    #[tokio::test]
    async fn chats_stored_before_search_are_found_in_their_own_workspace_only() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("old.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One'), ('w2', 'Two');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1'), ('w2', 'm2', 'ada@example.com', 'a1');
             INSERT INTO tasks VALUES ('w1', 't1', 'Ship the build', 'todo', 'a', 1);
             INSERT INTO task_versions (workspace_id, task_id, version, author_kind,
                 author_member, accepted_by, at) VALUES ('w1', 't1', 1, 'person', 'm1', 'm1', 5);
             INSERT INTO chats (workspace_id, chat_id, owner_member, title, machine_id,
                 machine_name, state, created_at, updated_at)
                 VALUES ('w1', 'c-1', 'm1', 'Fix the build', 'mc1', 'Laptop', 'open', 6, 6),
                        ('w2', 'c-2', 'm2', 'Fix the build', 'mc1', 'Laptop', 'open', 7, 7);
             INSERT INTO chat_events (workspace_id, chat_id, seq, body, at) VALUES
                 ('w1', 'c-1', 1, '{\"kind\":\"prompt\",\"request\":\"r-1\",\"text\":\"Why\\u0001 flaky\"}', 8),
                 ('w1', 'c-1', 2, 'not json, flaky', 9),
                 ('w2', 'c-2', 1, '{\"kind\":\"outside_prompt\",\"text\":\"Flaky deploy\"}', 10);";
        database_before(&path, steps::SEARCH, rows).await;

        let db = Db::open(&path).await.unwrap();
        let (one, two) = (
            db.workspace(&workspace_id("w1")).await.unwrap(),
            db.workspace(&workspace_id("w2")).await.unwrap(),
        );
        assert_eq!(found(&one, "build").await, ["c-1", "t1"]);
        assert_eq!(found(&one, "flaky").await, ["c-1/1"]);
        assert_eq!(found(&two, "flaky").await, ["c-2/1"]);
        assert_eq!(found(&two, "build").await, ["c-2"]);
        let bodies: Vec<String> =
            sqlx::query_scalar("SELECT body FROM search WHERE kind = 'message'")
                .fetch_all(&db.readers)
                .await
                .unwrap();
        assert!(
            bodies.iter().all(|body| !body.contains('\u{1}')),
            "{bodies:?}"
        );
    }

    #[tokio::test]
    async fn a_body_of_one_repeated_letter_is_found_with_a_short_snippet() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.add_doc(&doc("d1", "Notes", &"a".repeat(256 * 1024), &[]), &by(1))
            .await
            .unwrap();

        let (word, around) = marked(&one, "aaa").await;
        assert_eq!(word.chars().count(), 200);
        assert_eq!(around, format!("{word} …"));
    }
}
