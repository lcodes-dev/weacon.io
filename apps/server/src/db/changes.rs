use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{MemberId, TaskId, WorkspaceChange, WorkspaceUpdate};

use super::{Workspace, failed, stored, unreadable};

const MEMBER_JOINED: &str = "member_joined";
const TASK_CHANGED: &str = "task_changed";

type Row = (i64, String, String, Option<String>, Option<i64>);

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

    /// Records, inside the transaction that stores it, that `task` is now at `version`,
    /// through a change `member` made or accepted.
    pub(super) async fn task_changed(
        &self,
        transaction: &mut SqliteConnection,
        task: &TaskId,
        version: i64,
        member: &MemberId,
    ) -> Result<(), Fault> {
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id, task_id, task_version)
             SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5 FROM changes
             WHERE workspace_id = ?1",
        )
        .bind(self.id.as_str())
        .bind(TASK_CHANGED)
        .bind(member.as_str())
        .bind(task.as_str())
        .bind(version)
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
            "SELECT seq, kind, member_id, task_id, task_version FROM changes
             WHERE workspace_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(after)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|row| self.update(row))
        .collect()
    }

    fn update(&self, (seq, kind, member, task, version): Row) -> Result<WorkspaceUpdate, Fault> {
        let change = match (kind.as_str(), task, version) {
            (MEMBER_JOINED, None, None) => WorkspaceChange::MemberJoined {
                member: stored(member)?,
            },
            (TASK_CHANGED, Some(task), Some(version)) => WorkspaceChange::TaskChanged {
                task: stored(task)?,
                version: u64::try_from(version).map_err(|_| unreadable())?,
            },
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

    use super::*;
    use crate::test_support::{member_id, two_teams, workspace_id};

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
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id, task_id, task_version)
             VALUES ('w1', 1, 'task_changed', 'm1', 't1', 2),
                    ('w1', 2, 'task_changed', 'm1', 't1', NULL),
                    ('w1', 3, 'member_joined', 'm1', 't1', NULL)",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();

        let changed = WorkspaceUpdate {
            workspace: workspace_id("w1"),
            seq: 1,
            change: WorkspaceChange::TaskChanged {
                task: TaskId::try_from("t1".to_owned()).unwrap(),
                version: 2,
            },
        };
        assert_eq!(one.changes_since(0, 1).await.unwrap(), [changed]);
        for after in [1, 2] {
            let fault = one.changes_since(after, 1).await.unwrap_err();
            assert_eq!(fault.code().as_str(), "db.unreadable");
        }
    }
}
