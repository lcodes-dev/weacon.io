use weacon_observability::Fault;
use weacon_protocol::{MemberId, WorkspaceChange, WorkspaceUpdate};

use super::{Workspace, failed, stored, unreadable};

const MEMBER_JOINED: &str = "member_joined";

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

    /// Up to `limit` changes after `seq`, in order.
    pub(crate) async fn changes_since(
        &self,
        seq: u64,
        limit: u32,
    ) -> Result<Vec<WorkspaceUpdate>, Fault> {
        let after = i64::try_from(seq).unwrap_or(i64::MAX);
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT seq, kind, member_id FROM changes
             WHERE workspace_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(after)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(seq, kind, member)| self.update(seq, &kind, member))
        .collect()
    }

    fn update(&self, seq: i64, kind: &str, member: String) -> Result<WorkspaceUpdate, Fault> {
        let change = match kind {
            MEMBER_JOINED => WorkspaceChange::MemberJoined {
                member: stored(member)?,
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
}
