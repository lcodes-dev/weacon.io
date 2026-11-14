use sqlx::SqliteConnection;
use weacon_model::{Change, Doc, DocText, ReviseError, Revision};
use weacon_observability::Fault;
use weacon_protocol::{DocAnchors, DocId, DocRevision, DocSummary, code};

use super::{Workspace, author_columns, failed, stored, stored_author, unreadable};

type TextRow = (String, String, Option<String>);
type RevisionRow = (i64, String, String, String, i64);

impl Workspace {
    /// Stores a new doc, made by `change`. An id already in this workspace is `db.exists`.
    pub(crate) async fn add_doc(&self, doc: &Doc, change: &Change) -> Result<(), Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        sqlx::query("INSERT INTO docs (workspace_id, doc_id, title, version) VALUES (?, ?, ?, ?)")
            .bind(self.id.as_str())
            .bind(doc.id.as_str())
            .bind(doc.text.title().as_str())
            .bind(version_number(doc.version)?)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.record_doc(&mut transaction, doc, change).await?;
        transaction.commit().await.map_err(failed)
    }

    /// The docs, by title.
    pub(crate) async fn docs(&self) -> Result<Vec<DocSummary>, Fault> {
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT doc_id, title, version FROM docs WHERE workspace_id = ?
             ORDER BY title, doc_id",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, title, version)| {
            Ok(DocSummary {
                id: stored(id)?,
                title,
                version: stored_version(version)?.get(),
            })
        })
        .collect()
    }

    /// The doc `id` at `version`, or as it is now without one. A doc or a version there
    /// isn't is `doc.not_found`.
    pub(crate) async fn doc(&self, id: &DocId, version: Option<Revision>) -> Result<Doc, Fault> {
        let version = version
            .map(|version| i64::try_from(version.get()).map_err(|_| doc_not_found()))
            .transpose()?;
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.read_doc(&mut reader, id, version).await
    }

    /// Replaces the text of the doc `id` and gives the doc as it then is. One revised since
    /// `read_at` is `doc.stale`, and one there isn't is `doc.not_found`.
    pub(crate) async fn revise_doc(
        &self,
        id: &DocId,
        read_at: Revision,
        text: DocText,
        change: &Change,
    ) -> Result<Doc, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let current = self.read_doc(&mut transaction, id, None).await?;
        let revised = current.revise(read_at, text).map_err(refused)?;
        sqlx::query("UPDATE docs SET title = ?, version = ? WHERE workspace_id = ? AND doc_id = ?")
            .bind(revised.text.title().as_str())
            .bind(version_number(revised.version)?)
            .bind(self.id.as_str())
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.record_doc(&mut transaction, &revised, change).await?;
        transaction.commit().await.map_err(failed)?;
        Ok(revised)
    }

    /// The doc's newest `limit` revisions, newest first. A doc there isn't is
    /// `doc.not_found`.
    pub(crate) async fn doc_history(
        &self,
        id: &DocId,
        limit: u32,
    ) -> Result<Vec<DocRevision>, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        let known = sqlx::query("SELECT 1 FROM docs WHERE workspace_id = ? AND doc_id = ?")
            .bind(self.id.as_str())
            .bind(id.as_str())
            .fetch_optional(&mut *reader)
            .await
            .map_err(failed)?;
        known.ok_or_else(doc_not_found)?;
        sqlx::query_as::<_, RevisionRow>(
            "SELECT version, author_kind, author_member, accepted_by, at FROM doc_versions
             WHERE workspace_id = ? AND doc_id = ?
             ORDER BY version DESC LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .bind(limit)
        .fetch_all(&mut *reader)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(version, kind, author, accepted_by, at)| {
            Ok(DocRevision {
                version: stored_version(version)?.get(),
                author: stored_author(&kind, author)?,
                accepted_by: stored(accepted_by)?,
                at,
            })
        })
        .collect()
    }

    async fn read_doc(
        &self,
        connection: &mut SqliteConnection,
        id: &DocId,
        version: Option<i64>,
    ) -> Result<Doc, Fault> {
        let (version, (title, body, commit)) =
            sqlx::query_as::<_, (i64, String, String, Option<String>)>(
                "SELECT version, title, body, commit_id FROM doc_versions
             WHERE workspace_id = ?1 AND doc_id = ?2 AND version = COALESCE(?3,
                 (SELECT version FROM docs WHERE workspace_id = ?1 AND doc_id = ?2))",
            )
            .bind(self.id.as_str())
            .bind(id.as_str())
            .bind(version)
            .fetch_optional(&mut *connection)
            .await
            .map_err(failed)?
            .map(|(version, title, body, commit)| (version, (title, body, commit)))
            .ok_or_else(doc_not_found)?;
        let pins: Vec<String> = sqlx::query_scalar(
            "SELECT pin FROM doc_pins WHERE workspace_id = ? AND doc_id = ? AND version = ?
             ORDER BY position",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .bind(version)
        .fetch_all(&mut *connection)
        .await
        .map_err(failed)?;
        Ok(Doc {
            id: id.clone(),
            text: stored_text((title, body, commit), pins)?,
            version: stored_version(version)?,
        })
    }

    /// Records the revision `doc` is now at, its pins, who is behind it, and the change for
    /// the feed.
    async fn record_doc(
        &self,
        transaction: &mut SqliteConnection,
        doc: &Doc,
        change: &Change,
    ) -> Result<(), Fault> {
        let (kind, author) = author_columns(change.author()).ok_or_else(unknown)?;
        let version = version_number(doc.version)?;
        let anchors = doc.text.anchors();
        sqlx::query(
            "INSERT INTO doc_versions (workspace_id, doc_id, version, title, body, commit_id,
                 author_kind, author_member, accepted_by, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(doc.id.as_str())
        .bind(version)
        .bind(doc.text.title().as_str())
        .bind(doc.text.body())
        .bind(anchors.map(|anchors| anchors.commit.as_str()))
        .bind(kind)
        .bind(author.as_str())
        .bind(change.accepted_by().as_str())
        .bind(change.at())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        for (position, pin) in anchors.iter().flat_map(|anchors| &anchors.pins).enumerate() {
            sqlx::query(
                "INSERT INTO doc_pins (workspace_id, doc_id, version, position, pin)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(self.id.as_str())
            .bind(doc.id.as_str())
            .bind(version)
            .bind(i64::try_from(position).map_err(|_| unreadable())?)
            .bind(pin.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        self.doc_changed(transaction, &doc.id, version, change.accepted_by())
            .await
    }
}

fn stored_text((title, body, commit): TextRow, pins: Vec<String>) -> Result<DocText, Fault> {
    let anchors = commit
        .map(|commit| {
            Ok::<_, Fault>(DocAnchors {
                commit: stored(commit)?,
                pins: pins.into_iter().map(stored).collect::<Result<_, _>>()?,
            })
        })
        .transpose()?;
    DocText::new(stored(title)?, body, anchors).map_err(|_| unreadable())
}

pub(crate) fn doc_not_found() -> Fault {
    Fault::new(code!("doc.not_found"), "There is no such doc")
}

fn refused(error: ReviseError) -> Fault {
    match error {
        ReviseError::Stale { .. } => {
            Fault::new(code!("doc.stale"), "The doc has changed since it was read")
        }
        ReviseError::LastVersion => last_version(),
    }
}

/// An author from a newer peer, which this build can't store.
fn unknown() -> Fault {
    Fault::new(
        code!("doc.unknown_value"),
        "The doc has a value this version doesn't know",
    )
}

fn last_version() -> Fault {
    Fault::new(
        code!("doc.last_version"),
        "The doc can't be revised any more",
    )
}

fn version_number(version: Revision) -> Result<i64, Fault> {
    i64::try_from(version.get()).map_err(|_| last_version())
}

fn stored_version(version: i64) -> Result<Revision, Fault> {
    u64::try_from(version)
        .map(Revision::new)
        .map_err(|_| unreadable())
}

#[cfg(test)]
mod tests {
    use weacon_model::Title;
    use weacon_protocol::{Author, CommitId, DocPin, WorkspaceChange};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{member_id, two_teams};

    const NOW: i64 = 1_762_077_600;

    fn doc_id(id: &str) -> DocId {
        DocId::try_from(id.to_owned()).unwrap()
    }

    fn text(title: &str, body: &str, pinned: &[&str]) -> DocText {
        let anchors = (!pinned.is_empty()).then(|| DocAnchors {
            commit: CommitId::try_from("c".repeat(40)).unwrap(),
            pins: pinned
                .iter()
                .map(|anchor| DocPin::try_from(format!("{anchor}#0123456789abcdef")).unwrap())
                .collect(),
        });
        DocText::new(
            Title::try_from(title.to_owned()).unwrap(),
            body.to_owned(),
            anchors,
        )
        .unwrap()
    }

    fn linked() -> DocText {
        text(
            "Framing",
            "See [a](sym:a) and [b](area:b).",
            &["sym:a", "area:b"],
        )
    }

    fn new_doc(id: &str, title: &str) -> Doc {
        Doc::new(doc_id(id), text(title, "Plain.", &[]))
    }

    fn by(member: &str) -> Change {
        Change::by(member_id(member), NOW)
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    fn summary(doc: &Doc) -> DocSummary {
        DocSummary {
            id: doc.id.clone(),
            title: doc.text.title().as_str().to_owned(),
            version: doc.version.get(),
        }
    }

    #[tokio::test]
    async fn docs_are_listed_by_title_and_read_back_with_their_pins() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let pinned = Doc::new(doc_id("d1"), linked());
        for doc in [&pinned, &new_doc("d3", "Agents"), &new_doc("d2", "Agents")] {
            one.add_doc(doc, &by("m1")).await.unwrap();
        }

        assert_eq!(
            one.docs().await.unwrap(),
            [
                summary(&new_doc("d2", "Agents")),
                summary(&new_doc("d3", "Agents")),
                summary(&pinned),
            ]
        );
        assert_eq!(one.doc(&doc_id("d1"), None).await.unwrap(), pinned);
        assert_eq!(
            one.doc(&doc_id("d2"), Some(Revision::FIRST)).await.unwrap(),
            new_doc("d2", "Agents")
        );
    }

    #[tokio::test]
    async fn a_revision_is_stored_whole_and_the_old_ones_stay_readable() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let first = new_doc("d1", "Framing");
        one.add_doc(&first, &by("m1")).await.unwrap();
        let agents = Change::accepted(member_id("m1"), member_id("m3"), NOW + 1);

        let second = one
            .revise_doc(&first.id, Revision::FIRST, linked(), &agents)
            .await
            .unwrap();

        assert_eq!(
            second,
            Doc {
                text: linked(),
                version: Revision::new(2),
                ..first.clone()
            }
        );
        assert_eq!(one.doc(&first.id, None).await.unwrap(), second);
        assert_eq!(
            one.doc(&first.id, Some(Revision::FIRST)).await.unwrap(),
            first
        );
        assert_eq!(one.docs().await.unwrap(), [summary(&second)]);
        let history = one.doc_history(&first.id, 10).await.unwrap();
        assert_eq!(
            history,
            [
                DocRevision {
                    version: 2,
                    author: Author::Agent {
                        owner: member_id("m1")
                    },
                    accepted_by: member_id("m3"),
                    at: NOW + 1,
                },
                DocRevision {
                    version: 1,
                    author: Author::Person {
                        member: member_id("m1")
                    },
                    accepted_by: member_id("m1"),
                    at: NOW,
                },
            ]
        );
        let newest = history.first().cloned();
        assert_eq!(
            one.doc_history(&first.id, 1).await.unwrap(),
            Vec::from_iter(newest)
        );
    }

    #[tokio::test]
    async fn each_version_is_a_change_on_the_feed() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let first = new_doc("d1", "Framing");
        one.add_doc(&first, &by("m1")).await.unwrap();
        one.revise_doc(&first.id, Revision::FIRST, linked(), &by("m3"))
            .await
            .unwrap();

        let doc_changed = |version| WorkspaceChange::DocChanged {
            doc: first.id.clone(),
            version,
        };
        let feed: Vec<_> = one
            .changes_since(0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|update| (update.seq, update.change))
            .collect();
        assert_eq!(feed, [(1, doc_changed(1)), (2, doc_changed(2))]);
    }

    #[tokio::test]
    async fn a_stale_revision_is_refused_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let first = new_doc("d1", "Framing");
        one.add_doc(&first, &by("m1")).await.unwrap();
        let second = one
            .revise_doc(&first.id, Revision::FIRST, linked(), &by("m1"))
            .await
            .unwrap();

        let late = one
            .revise_doc(
                &first.id,
                Revision::FIRST,
                text("Late", "Late.", &[]),
                &by("m3"),
            )
            .await
            .unwrap_err();

        assert_eq!(code(&late), "doc.stale");
        assert_eq!(one.doc(&first.id, None).await.unwrap(), second);
        assert_eq!(one.doc_history(&first.id, 10).await.unwrap().len(), 2);
        assert_eq!(one.changes_since(0, 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn workspaces_keep_their_docs_apart() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        let doc = new_doc("d1", "Framing");
        one.add_doc(&doc, &by("m1")).await.unwrap();
        let id = &doc.id;

        assert!(two.docs().await.unwrap().is_empty());
        for refused in [
            two.doc(id, None).await.unwrap_err(),
            two.revise_doc(id, Revision::FIRST, linked(), &by("m2"))
                .await
                .unwrap_err(),
            two.doc_history(id, 10).await.unwrap_err(),
        ] {
            assert_eq!(code(&refused), "doc.not_found");
        }
        assert_eq!(
            code(&two.add_doc(&doc, &by("m1")).await.unwrap_err()),
            "db.failed"
        );
        two.add_doc(&doc, &by("m2")).await.unwrap();
        assert_eq!(
            code(&one.add_doc(&doc, &by("m1")).await.unwrap_err()),
            "db.exists"
        );
        assert_eq!(one.doc(id, None).await.unwrap(), doc);
        assert_eq!(two.changes_since(0, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_version_there_isnt_or_past_the_last_is_refused() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let doc = new_doc("d1", "Framing");
        one.add_doc(&doc, &by("m1")).await.unwrap();
        let too_far = Doc {
            version: Revision::new(u64::MAX),
            ..new_doc("d2", "Far")
        };

        let missing = one.doc(&doc.id, Some(Revision::new(2))).await.unwrap_err();
        let past = one
            .doc(&doc.id, Some(Revision::new(u64::MAX)))
            .await
            .unwrap_err();
        let added = one.add_doc(&too_far, &by("m1")).await.unwrap_err();

        assert_eq!(code(&missing), "doc.not_found");
        assert_eq!(code(&past), "doc.not_found");
        assert_eq!(code(&added), "doc.last_version");
        assert_eq!(
            code(&one.doc(&doc_id("d9"), None).await.unwrap_err()),
            "doc.not_found"
        );
        assert_eq!(one.docs().await.unwrap(), [summary(&doc)]);
    }

    #[tokio::test]
    async fn stored_values_are_checked_when_read() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        for (id, doc) in [
            ("d1", Doc::new(doc_id("d1"), linked())),
            ("d2", new_doc("d2", "Two")),
        ] {
            one.add_doc(&doc, &by("m1")).await.unwrap();
            assert_eq!(one.doc(&doc_id(id), None).await.unwrap(), doc);
        }
        sqlx::query(
            "UPDATE doc_pins SET pin = 'sym:x#0123456789abcdef'
                 WHERE workspace_id = 'w1' AND doc_id = 'd1' AND position = 0;
             UPDATE doc_versions SET author_kind = 'robot'
                 WHERE workspace_id = 'w1' AND doc_id = 'd2'",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();

        let unpinned = one.doc(&doc_id("d1"), None).await.unwrap_err();
        let robot = one.doc_history(&doc_id("d2"), 10).await.unwrap_err();

        assert_eq!(code(&unpinned), "db.unreadable");
        assert_eq!(code(&robot), "db.unreadable");
    }
}
