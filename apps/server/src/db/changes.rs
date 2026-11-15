use sqlx::SqliteConnection;
use weacon_model::Revision;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatId, DocId, MemberId, ProposalId, TaskId, WorkspaceChange, WorkspaceUpdate,
};

use super::{Workspace, failed, stored, stored_version, unreadable};

const MEMBER_JOINED: &str = "member_joined";
const TASK_CHANGED: &str = "task_changed";
const DOC_CHANGED: &str = "doc_changed";
const PROPOSAL_CHANGED: &str = "proposal_changed";
const CHAT_CHANGED: &str = "chat_changed";

/// A task or a doc, at the version it now has as stored, a proposal that was made or whose
/// state changed, or a chat that was made or whose turn began or ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum Versioned<'a> {
    Task(&'a TaskId, i64),
    Doc(&'a DocId, i64),
    Proposal(&'a ProposalId),
    Chat(&'a ChatId),
}

type Row = (
    i64,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
);

impl Workspace {
    /// Records that `member` joined, as the workspace's next change; false when they already
    /// had, which records nothing.
    pub(crate) async fn join(&self, member: &MemberId) -> Result<bool, Fault> {
        let joined = sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id)
             SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3 FROM changes WHERE workspace_id = ?1
             HAVING NOT EXISTS (SELECT 1 FROM changes
                 WHERE workspace_id = ?1 AND kind = ?2 AND member_id = ?3)",
        )
        .bind(self.id.as_str())
        .bind(MEMBER_JOINED)
        .bind(member.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(joined.rows_affected() == 1)
    }

    /// Records, inside the transaction that stores it, that `versioned` is at a new version,
    /// through a change `member` made or accepted.
    pub(super) async fn record_change(
        &self,
        transaction: &mut SqliteConnection,
        versioned: Versioned<'_>,
        member: &MemberId,
    ) -> Result<(), Fault> {
        let (kind, task, doc, proposal, chat) = match versioned {
            Versioned::Task(id, version) => {
                (TASK_CHANGED, Some((id.as_str(), version)), None, None, None)
            }
            Versioned::Doc(id, version) => {
                (DOC_CHANGED, None, Some((id.as_str(), version)), None, None)
            }
            Versioned::Proposal(id) => (PROPOSAL_CHANGED, None, None, Some(id.as_str()), None),
            Versioned::Chat(id) => (CHAT_CHANGED, None, None, None, Some(id.as_str())),
        };
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id, task_id, task_version,
                 doc_id, doc_version, proposal_id, chat_id)
             SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 FROM changes
             WHERE workspace_id = ?1",
        )
        .bind(self.id.as_str())
        .bind(kind)
        .bind(member.as_str())
        .bind(task.map(|(id, _)| id))
        .bind(task.map(|(_, version)| version))
        .bind(doc.map(|(id, _)| id))
        .bind(doc.map(|(_, version)| version))
        .bind(proposal)
        .bind(chat)
        .execute(transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Up to `limit` changes after `seq`, in order.
    pub(crate) async fn changes_since(
        &self,
        seq: u64,
        limit: u32,
    ) -> Result<Vec<WorkspaceUpdate>, Fault> {
        let after = i64::try_from(seq).unwrap_or(i64::MAX);
        sqlx::query_as::<_, Row>(
            "SELECT seq, kind, member_id, task_id, task_version, doc_id, doc_version, proposal_id,
                 chat_id
             FROM changes
             WHERE workspace_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(after)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|row| self.stored_update(row))
        .collect()
    }

    fn stored_update(&self, row: Row) -> Result<WorkspaceUpdate, Fault> {
        let (seq, kind, member, task, task_version, doc, doc_version, proposal, chat) = row;
        let version = |version| stored_version(version).map(Revision::get);
        let change = match (
            kind.as_str(),
            task,
            task_version,
            doc,
            doc_version,
            proposal,
            chat,
        ) {
            (MEMBER_JOINED, None, None, None, None, None, None) => WorkspaceChange::MemberJoined {
                member: stored(member)?,
            },
            (TASK_CHANGED, Some(task), Some(task_version), None, None, None, None) => {
                WorkspaceChange::TaskChanged {
                    task: stored(task)?,
                    version: version(task_version)?,
                }
            }
            (DOC_CHANGED, None, None, Some(doc), Some(doc_version), None, None) => {
                WorkspaceChange::DocChanged {
                    doc: stored(doc)?,
                    version: version(doc_version)?,
                }
            }
            (PROPOSAL_CHANGED, None, None, None, None, Some(proposal), None) => {
                WorkspaceChange::ProposalChanged {
                    proposal: stored(proposal)?,
                }
            }
            (CHAT_CHANGED, None, None, None, None, None, Some(chat)) => {
                WorkspaceChange::ChatChanged {
                    chat: stored(chat)?,
                }
            }
            // This server writes every kind it stores, so another is damage, not news.
            _ => return Err(unreadable()),
        };
        Ok(WorkspaceUpdate {
            workspace: self.id.clone(),
            seq: u64::try_from(seq).map_err(|_| unreadable())?,
            change,
        })
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use weacon_model::Attribution;

    use super::*;
    use crate::db::Db;
    use crate::db::schema::{database_before, steps};
    use crate::test_support::{member_id, new_task, two_teams, workspace_id};

    fn joined(seq: u64, member: &str) -> WorkspaceUpdate {
        WorkspaceUpdate {
            workspace: workspace_id("w1"),
            seq,
            change: WorkspaceChange::MemberJoined {
                member: member_id(member),
            },
        }
    }

    #[tokio::test]
    async fn a_member_joins_once_and_only_their_workspace_hears_of_it() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;

        assert!(one.join(&member_id("m1")).await.unwrap());
        assert!(!one.join(&member_id("m1")).await.unwrap());
        assert!(one.join(&member_id("m3")).await.unwrap());
        assert_eq!(
            one.changes_since(0, 10).await.unwrap(),
            [joined(1, "m1"), joined(2, "m3")]
        );
        assert_eq!(one.changes_since(1, 10).await.unwrap(), [joined(2, "m3")]);
        assert!(two.changes_since(0, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn changes_come_in_pages_and_an_unknown_kind_is_unreadable() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id)
             VALUES ('w1', 1, 'member_joined', 'm1'), ('w1', 2, 'member_joined', 'm1'),
                    ('w1', 3, 'x', 'm1')",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();

        assert_eq!(
            one.changes_since(0, 2).await.unwrap(),
            [joined(1, "m1"), joined(2, "m1")]
        );
        assert_eq!(
            one.changes_since(2, 2).await.unwrap_err().code().as_str(),
            "db.unreadable"
        );
    }

    #[tokio::test]
    async fn a_task_change_names_its_task_and_version_and_needs_both() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &Attribution::by(member_id("m1"), 0))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id, task_id, task_version)
             VALUES ('w1', 2, 'task_changed', 'm1', 't1', NULL),
                    ('w1', 3, 'member_joined', 'm1', 't1', 1)",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();
        let orphan = sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id, task_id, task_version)
             VALUES ('w1', 4, 'task_changed', 'm1', 't1', 2)",
        )
        .execute(&one.db.writer)
        .await;

        let changed = WorkspaceUpdate {
            workspace: workspace_id("w1"),
            seq: 1,
            change: WorkspaceChange::TaskChanged {
                task: task.id,
                version: 1,
            },
        };
        assert_eq!(one.changes_since(0, 1).await.unwrap(), [changed]);
        for after in [1, 2] {
            let fault = one.changes_since(after, 1).await.unwrap_err();
            assert_eq!(fault.code().as_str(), "db.unreadable");
        }
        orphan.unwrap_err();
    }

    #[tokio::test]
    async fn changes_from_before_docs_are_kept() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let rows = "INSERT INTO workspaces VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO tasks VALUES ('w1', 't1', 'Write', 'todo', 'V', 1);
             INSERT INTO task_versions VALUES ('w1', 't1', 1, 'person', 'm1', 'm1', 0);
             INSERT INTO changes VALUES
                 ('w1', 1, 'member_joined', 'm1', NULL, NULL),
                 ('w1', 2, 'task_changed', 'm1', 't1', 1)";
        database_before(&path, steps::DOCS, rows).await;

        let db = Db::open(&path).await.unwrap();
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();

        let task_changed = WorkspaceUpdate {
            workspace: workspace_id("w1"),
            seq: 2,
            change: WorkspaceChange::TaskChanged {
                task: TaskId::try_from("t1".to_owned()).unwrap(),
                version: 1,
            },
        };
        assert_eq!(
            w1.changes_since(0, 10).await.unwrap(),
            [joined(1, "m1"), task_changed]
        );
    }

    #[tokio::test]
    async fn changes_from_before_proposals_are_kept() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO tasks VALUES ('w1', 't1', 'Write', 'todo', 'V', 1);
             INSERT INTO task_versions (workspace_id, task_id, version, author_kind,
                 author_member, accepted_by, at) VALUES ('w1', 't1', 1, 'person', 'm1', 'm1', 0);
             INSERT INTO docs VALUES ('w1', 'd1', 1);
             INSERT INTO doc_versions (workspace_id, doc_id, version, title, body, author_kind,
                 author_member, accepted_by, at)
                 VALUES ('w1', 'd1', 1, 'Plan', '', 'person', 'm1', 'm1', 0);
             INSERT INTO changes VALUES
                 ('w1', 1, 'member_joined', 'm1', NULL, NULL, NULL, NULL),
                 ('w1', 2, 'task_changed', 'm1', 't1', 1, NULL, NULL),
                 ('w1', 3, 'doc_changed', 'm1', NULL, NULL, 'd1', 1)";
        database_before(&path, steps::PROPOSALS, rows).await;

        let db = Db::open(&path).await.unwrap();
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();

        let changes: Vec<_> = w1
            .changes_since(0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|update| (update.seq, update.change))
            .collect();
        assert_eq!(
            changes,
            [
                (1, joined(1, "m1").change),
                (
                    2,
                    WorkspaceChange::TaskChanged {
                        task: TaskId::try_from("t1".to_owned()).unwrap(),
                        version: 1,
                    }
                ),
                (
                    3,
                    WorkspaceChange::DocChanged {
                        doc: DocId::try_from("d1".to_owned()).unwrap(),
                        version: 1,
                    }
                ),
            ]
        );
    }

    #[tokio::test]
    async fn changes_from_before_chats_are_kept_with_every_reference() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO docs VALUES ('w1', 'd1', 1);
             INSERT INTO doc_versions (workspace_id, doc_id, version, title, body, author_kind,
                 author_member, accepted_by, at)
                 VALUES ('w1', 'd1', 1, 'Plan', '', 'person', 'm1', 'm1', 0);
             INSERT INTO proposals (workspace_id, proposal_id, kind, doc_id, change_hash, bytes,
                 author_member, agent, machine_id, state, at)
                 VALUES ('w1', 'p1', 'doc', 'd1', X'00', 0, 'm1', 'agent', 'mc-1', 'open', 0);
             INSERT INTO changes (workspace_id, seq, kind, member_id, doc_id, doc_version,
                 proposal_id) VALUES
                 ('w1', 1, 'doc_changed', 'm1', 'd1', 1, NULL),
                 ('w1', 2, 'proposal_changed', 'm1', NULL, NULL, 'p1')";
        database_before(&path, steps::CHATS, rows).await;

        let db = Db::open(&path).await.unwrap();
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();

        let changes: Vec<_> = w1
            .changes_since(0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|update| (update.seq, update.change))
            .collect();
        let doc = DocId::try_from("d1".to_owned()).unwrap();
        let proposal = ProposalId::try_from("p1".to_owned()).unwrap();
        assert_eq!(
            changes,
            [
                (1, WorkspaceChange::DocChanged { doc, version: 1 }),
                (2, WorkspaceChange::ProposalChanged { proposal }),
            ]
        );
        let references = [
            "chats",
            "doc_versions",
            "members",
            "proposals",
            "task_versions",
        ];
        assert_eq!(referenced(&db).await, references);
    }

    // The tables `changes` refers to.
    async fn referenced(db: &Db) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT DISTINCT \"table\" FROM pragma_foreign_key_list('changes') ORDER BY 1",
        )
        .fetch_all(&db.readers)
        .await
        .unwrap()
    }
}
