use sqlx::SqliteConnection;
use weacon_model::{Attribution, Doc, DocText, ReviseError, Revision};
use weacon_observability::Fault;
use weacon_protocol::{DocAnchors, DocId, DocRevision, DocSummary, code};

use super::changes::Versioned;
use super::{
    Workspace, author_columns, failed, stored, stored_author, stored_version, unreadable,
    version_number,
};

/// How many bytes of doc revisions a workspace may hold.
const DOC_QUOTA: i64 = 1 << 30;
/// What each revision counts besides its text: its rows, keys and feed entry.
const REVISION_OVERHEAD: usize = 256;

type VersionRow = (i64, String, String, Option<String>);
type RevisionRow = (i64, String, String, Option<String>, String, i64);

impl Workspace {
    /// Stores a new doc, made as `attribution` says. An id already in this workspace is
    /// `db.exists`.
    pub(crate) async fn add_doc(&self, doc: &Doc, attribution: &Attribution) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.insert_doc(&mut transaction, doc, attribution).await?;
        transaction.commit().await.map_err(failed)
    }

    /// [`Self::add_doc`], inside the caller's transaction.
    pub(super) async fn insert_doc(
        &self,
        transaction: &mut SqliteConnection,
        doc: &Doc,
        attribution: &Attribution,
    ) -> Result<(), Fault> {
        sqlx::query("INSERT INTO docs (workspace_id, doc_id, version) VALUES (?, ?, ?)")
            .bind(self.id.as_str())
            .bind(doc.id.as_str())
            .bind(version_number(doc.version, last_version)?)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.record_doc(transaction, doc, attribution).await
    }

    /// The docs, by title.
    pub(crate) async fn docs(&self) -> Result<Vec<DocSummary>, Fault> {
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT docs.doc_id, doc_versions.title, docs.version
             FROM docs JOIN doc_versions USING (workspace_id, doc_id, version)
             WHERE docs.workspace_id = ? ORDER BY doc_versions.title, docs.doc_id",
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
            .map(|version| version_number(version, doc_not_found))
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
        attribution: &Attribution,
    ) -> Result<Doc, Fault> {
        let mut transaction = self.db.write().await?;
        let revised = self
            .update_doc(&mut transaction, id, read_at, text, attribution)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(revised)
    }

    /// [`Self::revise_doc`], inside the caller's transaction.
    pub(super) async fn update_doc(
        &self,
        transaction: &mut SqliteConnection,
        id: &DocId,
        read_at: Revision,
        text: DocText,
        attribution: &Attribution,
    ) -> Result<Doc, Fault> {
        let current = self.read_doc(transaction, id, None).await?;
        let revised = current.revise(read_at, text).map_err(refused)?;
        sqlx::query("UPDATE docs SET version = ? WHERE workspace_id = ? AND doc_id = ?")
            .bind(version_number(revised.version, last_version)?)
            .bind(self.id.as_str())
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.record_doc(transaction, &revised, attribution).await?;
        Ok(revised)
    }

    /// The doc's newest `limit` revisions, newest first; `limit` is at least 1. A doc there
    /// isn't, which has none, is `doc.not_found`.
    pub(crate) async fn doc_history(
        &self,
        id: &DocId,
        limit: u32,
    ) -> Result<Vec<DocRevision>, Fault> {
        let revisions = sqlx::query_as::<_, RevisionRow>(
            "SELECT version, author_kind, author_member, agent, accepted_by, at FROM doc_versions
             WHERE workspace_id = ? AND doc_id = ? ORDER BY version DESC LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?;
        if revisions.is_empty() {
            return Err(doc_not_found());
        }
        revisions
            .into_iter()
            .map(|(version, kind, author, agent, accepted_by, at)| {
                Ok(DocRevision {
                    version: stored_version(version)?.get(),
                    author: stored_author(&kind, author, agent.as_deref())?,
                    accepted_by: stored(accepted_by)?,
                    at,
                })
            })
            .collect()
    }

    pub(super) async fn read_doc(
        &self,
        connection: &mut SqliteConnection,
        id: &DocId,
        version: Option<i64>,
    ) -> Result<Doc, Fault> {
        let (version, title, body, commit) = sqlx::query_as::<_, VersionRow>(
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
        let anchors = match commit {
            Some(commit) => Some(DocAnchors {
                commit: stored(commit)?,
                pins: pins.into_iter().map(stored).collect::<Result<_, _>>()?,
            }),
            None => None,
        };
        Ok(Doc {
            id: id.clone(),
            text: DocText::new(stored(title)?, body, anchors).map_err(|_| unreadable())?,
            version: stored_version(version)?,
        })
    }

    /// Counts `size` more bytes against the workspace's quota, or refuses a revision it has
    /// no room left for.
    pub(super) async fn charge(
        &self,
        transaction: &mut SqliteConnection,
        size: i64,
    ) -> Result<(), Fault> {
        let charged = sqlx::query(
            "UPDATE workspaces SET doc_bytes = doc_bytes + ?2
             WHERE workspace_id = ?1 AND doc_bytes + ?2 <= ?3",
        )
        .bind(self.id.as_str())
        .bind(size)
        .bind(DOC_QUOTA)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if charged.rows_affected() == 0 {
            return Err(quota_full());
        }
        Ok(())
    }

    /// Records the revision `doc` is now at, its pins, who is behind it, and the change for
    /// the feed.
    async fn record_doc(
        &self,
        transaction: &mut SqliteConnection,
        doc: &Doc,
        attribution: &Attribution,
    ) -> Result<(), Fault> {
        let (kind, author, agent) = author_columns(attribution.author()).ok_or_else(unknown)?;
        let version = version_number(doc.version, last_version)?;
        let anchors = doc.text.anchors();
        let size = size(&doc.text, agent.as_deref());
        self.charge(transaction, size).await?;
        sqlx::query(
            "INSERT INTO doc_versions (workspace_id, doc_id, version, title, body, commit_id,
                 author_kind, author_member, agent, accepted_by, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(doc.id.as_str())
        .bind(version)
        .bind(doc.text.title().as_str())
        .bind(doc.text.body())
        .bind(anchors.map(|anchors| anchors.commit.as_str()))
        .bind(kind)
        .bind(author.as_str())
        .bind(agent)
        .bind(attribution.accepted_by().as_str())
        .bind(attribution.at())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        let pins = anchors.iter().flat_map(|anchors| &anchors.pins);
        // The model holds at most a few hundred pins, so every position fits.
        for (position, pin) in (0_i64..).zip(pins) {
            sqlx::query(
                "INSERT INTO doc_pins (workspace_id, doc_id, version, position, pin)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(self.id.as_str())
            .bind(doc.id.as_str())
            .bind(version)
            .bind(position)
            .bind(pin.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        let versioned = Versioned::Doc(&doc.id, version);
        self.record_change(transaction, versioned, attribution.accepted_by())
            .await
    }
}

/// The bytes a revision of `text` by an author whose agent is stored as `agent` counts: its
/// title, body, commit, pins and agent, and its rows.
fn size(text: &DocText, agent: Option<&str>) -> i64 {
    let anchors = text.anchors();
    let commit = anchors.map_or(0, |anchors| anchors.commit.as_str().len());
    let pins: usize = anchors
        .iter()
        .flat_map(|anchors| &anchors.pins)
        .map(|pin| pin.as_str().len())
        .sum();
    let agent = agent.map_or(0, str::len);
    let size =
        REVISION_OVERHEAD + text.title().as_str().len() + text.body().len() + commit + pins + agent;
    i64::try_from(size).unwrap_or(i64::MAX)
}

#[cfg(test)]
impl Workspace {
    /// Fills the workspace's quota, for tests of what a full one answers.
    pub(crate) async fn fill_doc_quota(&self) {
        self.set_doc_bytes(DOC_QUOTA).await;
    }

    /// Says the workspace holds `bytes` of docs.
    pub(crate) async fn set_doc_bytes(&self, bytes: i64) {
        sqlx::query("UPDATE workspaces SET doc_bytes = ? WHERE workspace_id = ?")
            .bind(bytes)
            .bind(self.id.as_str())
            .execute(&self.db.writer)
            .await
            .unwrap();
    }
}

fn quota_full() -> Fault {
    Fault::new(
        code!("doc.quota_full"),
        "The workspace has no room left for docs",
    )
}

fn doc_not_found() -> Fault {
    Fault::new(code!("doc.not_found"), "There is no such doc")
}

fn refused(error: ReviseError) -> Fault {
    match error {
        ReviseError::Stale => {
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

#[cfg(test)]
mod tests {
    use weacon_model::Title;
    use weacon_protocol::{AgentKind, AgentOwner, Author, CommitId, DocPin, WorkspaceChange};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;
    use crate::db::schema::{database_before, steps};
    use crate::db::test_support::{NOW, by, code};
    use crate::test_support::{agent, member_id, two_teams, workspace_id};

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
        let codex = agent(AgentKind::Codex, AgentOwner::Shared, Some("Build box"));
        let agents = Attribution::accepted(member_id("m1"), Some(codex), member_id("m3"), NOW + 1);

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
                    author: agents.author().clone(),
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
            ("d3", new_doc("d3", "Three")),
            ("d4", new_doc("d4", "Four")),
        ] {
            one.add_doc(&doc, &by("m1")).await.unwrap();
            assert_eq!(one.doc(&doc_id(id), None).await.unwrap(), doc);
        }
        sqlx::query(
            "UPDATE doc_pins SET pin = 'sym:x#0123456789abcdef'
                 WHERE workspace_id = 'w1' AND doc_id = 'd1' AND position = 0;
             UPDATE doc_versions SET author_kind = 'robot'
                 WHERE workspace_id = 'w1' AND doc_id = 'd2';
             UPDATE doc_versions SET author_kind = 'agent', agent = '{'
                 WHERE workspace_id = 'w1' AND doc_id = 'd3';
             UPDATE doc_versions SET agent = '{\"kind\":\"codex\",\"owner\":{\"kind\":\"shared\"}}'
                 WHERE workspace_id = 'w1' AND doc_id = 'd4'",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();

        let unpinned = one.doc(&doc_id("d1"), None).await.unwrap_err();
        let mut unreadable = Vec::new();
        for id in ["d2", "d3", "d4"] {
            let history = one.doc_history(&doc_id(id), 10).await.unwrap_err();
            unreadable.push(code(&history).to_owned());
        }

        assert_eq!(code(&unpinned), "db.unreadable");
        assert_eq!(unreadable, ["db.unreadable"; 3]);
    }

    #[tokio::test]
    async fn an_agent_from_before_agents_were_named_reads_as_none_and_a_newer_one_is_refused() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO docs VALUES ('w1', 'd1', 1);
             INSERT INTO doc_versions (workspace_id, doc_id, version, title, body, author_kind,
                 author_member, accepted_by, at)
                 VALUES ('w1', 'd1', 1, 'Framing', 'Plain.', 'agent', 'm1', 'm1', 0);";
        database_before(&path, steps::VERSION_AGENTS, rows).await;
        let db = Db::open(&path).await.unwrap();
        let one = db.workspace(&workspace_id("w1")).await.unwrap();
        let newer = agent(AgentKind::Newer, AgentOwner::Shared, None);
        let agents = Attribution::accepted(member_id("m1"), Some(newer), member_id("m1"), NOW);

        let refused = one
            .revise_doc(&doc_id("d1"), Revision::FIRST, linked(), &agents)
            .await
            .unwrap_err();

        let history = one.doc_history(&doc_id("d1"), 10).await.unwrap();
        let agent = Author::Agent {
            owner: member_id("m1"),
            agent: None,
        };
        assert_eq!(
            history
                .iter()
                .map(|revision| &revision.author)
                .collect::<Vec<_>>(),
            [&agent]
        );
        assert_eq!(code(&refused), "doc.unknown_value");
    }

    async fn doc_bytes(workspace: &Workspace) -> i64 {
        sqlx::query_scalar("SELECT doc_bytes FROM workspaces WHERE workspace_id = ?")
            .bind(workspace.id.as_str())
            .fetch_one(&workspace.db.readers)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn each_revision_counts_its_bytes_and_its_rows_toward_the_quota() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        one.add_doc(&Doc::new(doc_id("d1"), linked()), &by("m1"))
            .await
            .unwrap();
        one.add_doc(&new_doc("d2", "Two"), &by("m1")).await.unwrap();
        let codex = agent(AgentKind::Codex, AgentOwner::Shared, None);
        let agents = Attribution::accepted(member_id("m1"), Some(codex), member_id("m1"), NOW);
        one.add_doc(&new_doc("d3", "Three"), &agents).await.unwrap();

        let pinned = 256
            + "Framing".len()
            + "See [a](sym:a) and [b](area:b).".len()
            + 40
            + "sym:a#0123456789abcdef".len()
            + "area:b#0123456789abcdef".len();
        let plain = 256 + "Two".len() + "Plain.".len();
        let agent = r#"{"kind":"codex","owner":{"kind":"shared"}}"#;
        let by_agent = 256 + "Three".len() + "Plain.".len() + agent.len();
        assert_eq!(
            doc_bytes(&one).await,
            i64::try_from(pinned + plain + by_agent).unwrap()
        );
        assert_eq!(doc_bytes(&two).await, 0);
    }

    #[tokio::test]
    async fn a_full_workspace_keeps_its_docs_readable_but_takes_no_more() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        let first = new_doc("d1", "Framing");
        one.add_doc(&first, &by("m1")).await.unwrap();
        let next = linked();
        one.set_doc_bytes(DOC_QUOTA - size(&next, None)).await;

        let fits = one
            .revise_doc(&first.id, Revision::FIRST, next, &by("m1"))
            .await
            .unwrap();
        let revised = one
            .revise_doc(
                &first.id,
                fits.version,
                text("More", "More.", &[]),
                &by("m1"),
            )
            .await
            .unwrap_err();
        let made = one
            .add_doc(&new_doc("d2", "Two"), &by("m1"))
            .await
            .unwrap_err();

        assert_eq!(code(&revised), "doc.quota_full");
        assert_eq!(code(&made), "doc.quota_full");
        assert_eq!(doc_bytes(&one).await, DOC_QUOTA);
        assert_eq!(one.doc(&first.id, None).await.unwrap(), fits);
        assert_eq!(one.docs().await.unwrap(), [summary(&fits)]);
        assert_eq!(one.doc_history(&first.id, 10).await.unwrap().len(), 2);
        assert_eq!(one.changes_since(0, 10).await.unwrap().len(), 2);
        two.add_doc(&new_doc("d2", "Two"), &by("m2")).await.unwrap();
    }

    #[tokio::test]
    async fn revisions_from_before_the_quota_count_as_the_server_counts_them() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let rows = format!(
            "INSERT INTO workspaces VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO docs VALUES ('w1', 'd1', 1), ('w1', 'd2', 1);
             INSERT INTO doc_versions VALUES
                 ('w1', 'd1', 1, 'Framing', 'See [a](sym:a) and [b](area:b).', '{commit}',
                  'person', 'm1', 'm1', 0),
                 ('w1', 'd2', 1, 'Ünïcode', 'Plain.', NULL, 'person', 'm1', 'm1', 0);
             INSERT INTO doc_pins VALUES
                 ('w1', 'd1', 1, 0, 'sym:a#0123456789abcdef'),
                 ('w1', 'd1', 1, 1, 'area:b#0123456789abcdef');
             ",
            commit = "c".repeat(40),
        );
        database_before(&path, steps::DOC_BYTES, &rows).await;

        let db = Db::open(&path).await.unwrap();
        let one = db.workspace(&workspace_id("w1")).await.unwrap();

        let unicode = text("Ünïcode", "Plain.", &[]);
        assert_eq!(
            doc_bytes(&one).await,
            size(&linked(), None) + size(&unicode, None)
        );
    }
}
