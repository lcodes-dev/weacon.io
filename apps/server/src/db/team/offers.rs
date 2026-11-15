//! The invitations to an account's address, which only that account sees and takes up.

use weacon_observability::Fault;
use weacon_protocol::{InvitationOffer, MemberId, WorkspaceId, WorkspaceSummary};

use super::super::changes::Versioned;
use super::super::{Account, Workspace, failed, stored};
use crate::email::Email;

impl Account {
    /// The live invitations to the account's address, soonest to end first.
    pub(crate) async fn invitations(&self, now: i64) -> Result<Vec<InvitationOffer>, Fault> {
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT w.workspace_id, w.name, i.expires_at
             FROM accounts AS a JOIN invitations AS i ON i.email = a.email
             JOIN workspaces AS w ON w.workspace_id = i.workspace_id
             WHERE a.account_id = ? AND i.expires_at > ? ORDER BY i.expires_at, w.workspace_id",
        )
        .bind(self.id.as_str())
        .bind(now)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(workspace, name, expires_at)| {
            Ok(InvitationOffer {
                workspace: stored(workspace)?,
                name,
                expires_at,
            })
        })
        .collect()
    }

    /// Takes up the live invitation to `workspace` at `now`, as `member` unless the address was
    /// a member there before, and gives the workspace as its member sees it; `None` when there is
    /// no such invitation.
    pub(crate) async fn accept(
        &self,
        workspace: &WorkspaceId,
        member: &MemberId,
        now: i64,
    ) -> Result<Option<WorkspaceSummary>, Fault> {
        let mut transaction = self.db.write().await?;
        let found = sqlx::query_scalar::<_, String>(
            "SELECT a.email FROM accounts AS a JOIN invitations AS i ON i.email = a.email
             WHERE a.account_id = ? AND i.workspace_id = ? AND i.expires_at > ?",
        )
        .bind(self.id.as_str())
        .bind(workspace.as_str())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let Some(email) = found else {
            return Ok(None);
        };
        let email = Email::parse(&email).map_err(|_| super::super::unreadable())?;
        let place = Workspace::new(self.db.clone(), workspace.clone());
        let member = place.link(&mut transaction, member, &email).await?;
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
    use crate::db::team::INVITATION_LIFETIME;
    use crate::db::test_support::{NOW, id};
    use crate::db::{Db, NewWorkspace};
    use crate::test_support::{email, member_id, workspace_id};

    // ada owns w1 as o1 and w2 as o2; cy, with an account, is invited to w1 and bo, without one,
    // to w2.
    async fn invited(dir: &TempDir) -> Db {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        for (account, address) in [("a-ada", "ada@example.com"), ("a-cy", "cy@example.com")] {
            db.add_account(&id(account), &email(address)).await.unwrap();
        }
        let invites = [
            ("w1", "o1", "cy@example.com"),
            ("w2", "o2", "bo@example.com"),
        ];
        for (workspace, owner, address) in invites {
            let new = NewWorkspace {
                id: workspace_id(workspace),
                name: workspace.to_uppercase(),
                owner: member_id(owner),
            };
            db.account(id("a-ada")).create_workspace(new).await.unwrap();
            let place = db.workspace(&workspace_id(workspace)).await.unwrap();
            place
                .invite(&member_id(owner), &email(address), NOW)
                .await
                .unwrap();
        }
        db
    }

    fn offer(workspace: &str) -> InvitationOffer {
        InvitationOffer {
            workspace: workspace_id(workspace),
            name: workspace.to_uppercase(),
            expires_at: NOW + INVITATION_LIFETIME,
        }
    }

    #[tokio::test]
    async fn an_account_sees_only_the_invitations_to_its_own_address_while_they_last() {
        let dir = TempDir::new().unwrap();
        let db = invited(&dir).await;
        let (cy, ada) = (db.account(id("a-cy")), db.account(id("a-ada")));
        let (ends, i1) = (NOW + INVITATION_LIFETIME, member_id("i1"));

        assert_eq!(cy.invitations(NOW).await.unwrap(), [offer("w1")]);
        assert!(ada.invitations(NOW).await.unwrap().is_empty());
        assert!(cy.invitations(ends).await.unwrap().is_empty());
        assert_eq!(
            cy.accept(&workspace_id("w1"), &i1, ends).await.unwrap(),
            None
        );
        assert_eq!(
            cy.accept(&workspace_id("w2"), &i1, NOW).await.unwrap(),
            None
        );
        let bo: AccountId = id("a-bo");
        db.add_account(&bo, &email("bo@example.com")).await.unwrap();
        assert_eq!(
            db.account(bo).invitations(NOW).await.unwrap(),
            [offer("w2")]
        );
    }

    #[tokio::test]
    async fn taking_up_an_invitation_makes_a_member_once_and_a_past_one_again() {
        let dir = TempDir::new().unwrap();
        let db = invited(&dir).await;
        let (cy, i1, o1) = (db.account(id("a-cy")), member_id("i1"), member_id("o1"));
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();
        let before = w1.changes_since(0, 100).await.unwrap().len();

        let joined = cy
            .accept(&workspace_id("w1"), &i1, NOW)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(joined.role, WorkspaceRole::Member);
        assert_eq!(joined.member, Some(i1.clone()));
        assert_eq!(cy.workspaces().await.unwrap(), [joined]);
        assert!(cy.invitations(NOW).await.unwrap().is_empty());
        assert!(w1.invitations(NOW).await.unwrap().is_empty());
        assert_eq!(w1.changes_since(0, 100).await.unwrap().len(), before + 1);
        assert_eq!(
            cy.accept(&workspace_id("w1"), &i1, NOW).await.unwrap(),
            None
        );
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        assert_eq!(w2.invitations(NOW).await.unwrap().len(), 1);
        // Removed, then invited again, the address is the member it was.
        w1.remove(&o1, &i1).await.unwrap();
        assert!(cy.workspaces().await.unwrap().is_empty());
        w1.invite(&o1, &email("cy@example.com"), NOW).await.unwrap();
        let again = cy.accept(&workspace_id("w1"), &member_id("i9"), NOW).await;
        assert_eq!(again.unwrap().unwrap().member, Some(i1));
    }
}
