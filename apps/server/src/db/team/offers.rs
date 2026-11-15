//! The invitations to an account's address, which only that account sees and takes up.

use weacon_observability::Fault;
use weacon_protocol::{MemberId, WorkspaceId, WorkspaceSummary};

use super::super::changes::Versioned;
use super::super::{Account, Workspace, failed, stored};

/// A live invitation to the account's address, with the member who made it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) workspace: WorkspaceId,
    pub(crate) name: String,
    pub(crate) invited_by: MemberId,
    pub(crate) expires_at: i64,
}

/// The account `?1`'s live invitations at time `?2`, by its address.
macro_rules! live {
    () => {
        "accounts AS a JOIN members AS m ON m.email = a.email
         JOIN invitations AS i ON i.workspace_id = m.workspace_id AND i.member_id = m.member_id
         WHERE a.account_id = ?1 AND m.account_id IS NULL AND i.expires_at > ?2"
    };
}

impl Account {
    /// The live invitations to the account's address, soonest to end first.
    pub(crate) async fn invitations(&self, now: i64) -> Result<Vec<Offer>, Fault> {
        sqlx::query_as::<_, (String, String, String, i64)>(concat!(
            "SELECT w.workspace_id, w.name, i.invited_by, i.expires_at FROM workspaces AS w JOIN ",
            live!(),
            " AND w.workspace_id = m.workspace_id ORDER BY i.expires_at, m.rowid"
        ))
        .bind(self.id.as_str())
        .bind(now)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(workspace, name, invited_by, expires_at)| {
            Ok(Offer {
                workspace: stored(workspace)?,
                name,
                invited_by: stored(invited_by)?,
                expires_at,
            })
        })
        .collect()
    }

    /// Takes up the live invitation to `workspace`, and gives the workspace as its new member
    /// sees it; `None` when there is none.
    pub(crate) async fn accept(
        &self,
        workspace: &WorkspaceId,
        now: i64,
    ) -> Result<Option<WorkspaceSummary>, Fault> {
        let mut transaction = self.db.write().await?;
        let found = sqlx::query_scalar::<_, String>(concat!(
            "SELECT m.member_id FROM ",
            live!(),
            " AND m.workspace_id = ?3"
        ))
        .bind(self.id.as_str())
        .bind(now)
        .bind(workspace.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let Some(member) = found else {
            return Ok(None);
        };
        let member: MemberId = stored(member)?;
        sqlx::query(
            "UPDATE members SET account_id = ?, joined = 1 WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(self.id.as_str())
        .bind(workspace.as_str())
        .bind(member.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        sqlx::query(
            "UPDATE invitations SET expires_at = 0 WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(workspace.as_str())
        .bind(member.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        let place = Workspace::new(self.db.clone(), workspace.clone());
        place
            .record_change(&mut transaction, Versioned::Members, &member)
            .await?;
        transaction.commit().await.map_err(failed)?;
        place.summary(&member).await
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AccountId, WorkspaceRole};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::team::{INVITATION_LIFETIME, NewInvitation};
    use crate::db::test_support::{NOW, id};
    use crate::db::{Db, NewWorkspace};
    use crate::test_support::{email, member_id, workspace_id};

    // ada owns w1 as o1 and w2 as o2; cy, with an account, is invited to w1 as i1 and bo, without
    // one, to w2 as i2.
    async fn invited(dir: &TempDir) -> Db {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        for (account, address) in [("a-ada", "ada@example.com"), ("a-cy", "cy@example.com")] {
            db.add_account(&id(account), &email(address)).await.unwrap();
        }
        let invites = [
            ("w1", "o1", "i1", "cy@example.com"),
            ("w2", "o2", "i2", "bo@example.com"),
        ];
        for (workspace, owner, member, address) in invites {
            let new = NewWorkspace {
                id: workspace_id(workspace),
                name: workspace.to_uppercase(),
                owner: member_id(owner),
            };
            db.account(id("a-ada")).create_workspace(new).await.unwrap();
            let place = db.workspace(&workspace_id(workspace)).await.unwrap();
            let invitation = NewInvitation {
                member: &member_id(member),
                by: &member_id(owner),
                email: &email(address),
                now: NOW,
            };
            place.invite(&invitation).await.unwrap();
        }
        db
    }

    fn offer(workspace: &str, by: &str) -> Offer {
        Offer {
            workspace: workspace_id(workspace),
            name: workspace.to_uppercase(),
            invited_by: member_id(by),
            expires_at: NOW + INVITATION_LIFETIME,
        }
    }

    #[tokio::test]
    async fn an_account_sees_only_the_invitations_to_its_own_address_while_they_last() {
        let dir = TempDir::new().unwrap();
        let db = invited(&dir).await;
        let (cy, ada) = (db.account(id("a-cy")), db.account(id("a-ada")));
        let ends = NOW + INVITATION_LIFETIME;

        assert_eq!(cy.invitations(NOW).await.unwrap(), [offer("w1", "o1")]);
        assert!(ada.invitations(NOW).await.unwrap().is_empty());
        assert!(cy.invitations(ends).await.unwrap().is_empty());
        assert_eq!(cy.accept(&workspace_id("w1"), ends).await.unwrap(), None);
        assert_eq!(cy.accept(&workspace_id("w2"), NOW).await.unwrap(), None);
        let bo: AccountId = id("a-bo");
        db.add_account(&bo, &email("bo@example.com")).await.unwrap();
        assert_eq!(
            db.account(bo).invitations(NOW).await.unwrap(),
            [offer("w2", "o2")]
        );
    }

    #[tokio::test]
    async fn taking_up_an_invitation_makes_a_member_once() {
        let dir = TempDir::new().unwrap();
        let db = invited(&dir).await;
        let cy = db.account(id("a-cy"));
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();
        let before = w1.changes_since(0, 100).await.unwrap().len();

        let joined = cy.accept(&workspace_id("w1"), NOW).await.unwrap().unwrap();

        assert_eq!(joined.role, WorkspaceRole::Member);
        assert_eq!(joined.member, Some(member_id("i1")));
        assert_eq!(cy.workspaces().await.unwrap(), [joined]);
        assert!(cy.invitations(NOW).await.unwrap().is_empty());
        assert!(w1.invitations(NOW).await.unwrap().is_empty());
        assert_eq!(w1.changes_since(0, 100).await.unwrap().len(), before + 1);
        assert_eq!(cy.accept(&workspace_id("w1"), NOW).await.unwrap(), None);
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        assert_eq!(w2.invitations(NOW).await.unwrap().len(), 1);
        // Once removed, the invitation it took up gives no way back.
        w1.remove(&member_id("o1"), &member_id("i1")).await.unwrap();
        assert_eq!(cy.accept(&workspace_id("w1"), NOW).await.unwrap(), None);
        assert!(cy.workspaces().await.unwrap().is_empty());
    }
}
