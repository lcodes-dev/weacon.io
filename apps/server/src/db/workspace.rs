use weacon_observability::Fault;
use weacon_protocol::{MemberId, WorkspaceId};

use super::{Db, failed, stored, unreadable};
use crate::email::Email;

/// One workspace's view of the database: every query it runs is limited to that workspace.
/// Only the `db` module makes one, after checking that the workspace exists.
#[derive(Debug)]
pub(crate) struct Workspace {
    pub(super) db: Db,
    pub(super) id: WorkspaceId,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) id: MemberId,
    pub(crate) email: Email,
}

impl Workspace {
    pub(super) const fn new(db: Db, id: WorkspaceId) -> Self {
        Self { db, id }
    }

    pub(crate) const fn id(&self) -> &WorkspaceId {
        &self.id
    }

    /// Adds a member; an id or email already in this workspace is `db.exists`.
    pub(crate) async fn add_member(&self, id: &MemberId, email: &Email) -> Result<(), Fault> {
        sqlx::query("INSERT INTO members (workspace_id, member_id, email) VALUES (?, ?, ?)")
            .bind(self.id.as_str())
            .bind(id.as_str())
            .bind(email.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// The members, in the order they were added.
    pub(crate) async fn members(&self) -> Result<Vec<Member>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT member_id, email FROM members WHERE workspace_id = ? ORDER BY rowid",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, email)| {
            Ok(Member {
                id: stored(id)?,
                email: Email::parse(&email).map_err(|_| unreadable())?,
            })
        })
        .collect()
    }
}
