//! Who is in a workspace: its owner invites and removes people, and members leave.

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, MemberId, WorkspaceRole, code};

use super::changes::Versioned;
use super::sign_in::insert_account;
use super::workspace::stored_role;
use super::{Workspace, failed, unreadable};
use crate::email::Email;

mod offers;

/// How long an invitation lasts, in seconds.
pub(crate) const INVITATION_LIFETIME: i64 = 7 * 24 * 60 * 60;

/// A live invitation, as the workspace's owner sees it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvitationRow {
    pub(crate) email: Email,
    pub(crate) expires_at: i64,
}

impl Workspace {
    /// Invites `email` at `now`, if `by` is the owner and the address is neither a member nor
    /// invited already. An address with no account gets `account`, so it can sign in to accept.
    pub(crate) async fn invite(
        &self,
        by: &MemberId,
        account: &AccountId,
        email: &Email,
        now: i64,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.owner_only(&mut transaction, by).await?;
        let member = sqlx::query(
            "SELECT 1 FROM members WHERE workspace_id = ? AND email = ? AND account_id IS NOT NULL",
        )
        .bind(self.id.as_str())
        .bind(email.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        if member.is_some() {
            return Err(member_exists());
        }
        let invited = sqlx::query(
            "INSERT INTO invitations (workspace_id, email, expires_at) VALUES (?1, ?2, ?3 + ?4)
             ON CONFLICT (workspace_id, email) DO UPDATE SET expires_at = excluded.expires_at
                 WHERE expires_at <= ?3",
        )
        .bind(self.id.as_str())
        .bind(email.as_str())
        .bind(now)
        .bind(INVITATION_LIFETIME)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if invited.rows_affected() == 0 {
            return Err(invitation_exists());
        }
        insert_account(&mut transaction, account.as_str(), email).await?;
        self.record_change(&mut transaction, Versioned::Members, by)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Ends the live invitation to `email`, if `by` is the owner.
    pub(crate) async fn revoke(&self, by: &MemberId, email: &Email, now: i64) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.owner_only(&mut transaction, by).await?;
        let ended = sqlx::query(
            "DELETE FROM invitations WHERE workspace_id = ? AND email = ? AND expires_at > ?",
        )
        .bind(self.id.as_str())
        .bind(email.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if ended.rows_affected() == 0 {
            return Err(not_found());
        }
        self.record_change(&mut transaction, Versioned::Members, by)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Ends `member`'s membership, if `by` is the owner and `member` someone else.
    pub(crate) async fn remove(&self, by: &MemberId, member: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.owner_only(&mut transaction, by).await?;
        if self.current_role(&mut transaction, member).await? != Some(WorkspaceRole::Member) {
            return Err(not_found());
        }
        self.end_membership(&mut transaction, member).await?;
        self.record_change(&mut transaction, Versioned::Members, by)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Ends `member`'s own membership; the owner's can't end.
    pub(crate) async fn leave(&self, member: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        match self.current_role(&mut transaction, member).await? {
            None => return Err(not_found()),
            Some(WorkspaceRole::Member) => {}
            Some(_) => return Err(not_allowed()),
        }
        self.end_membership(&mut transaction, member).await?;
        self.record_change(&mut transaction, Versioned::Members, member)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// The live invitations, soonest to end first.
    pub(crate) async fn invitations(&self, now: i64) -> Result<Vec<InvitationRow>, Fault> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT email, expires_at FROM invitations
             WHERE workspace_id = ? AND expires_at > ? ORDER BY expires_at, email",
        )
        .bind(self.id.as_str())
        .bind(now)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(email, expires_at)| {
            Ok(InvitationRow {
                email: Email::parse(&email).map_err(|_| unreadable())?,
                expires_at,
            })
        })
        .collect()
    }

    /// Refuses `member` unless they are the owner.
    async fn owner_only(
        &self,
        connection: &mut SqliteConnection,
        member: &MemberId,
    ) -> Result<(), Fault> {
        match self.current_role(connection, member).await? {
            Some(WorkspaceRole::Owner) => Ok(()),
            _ => Err(not_allowed()),
        }
    }

    /// `member`'s role while they are a member.
    async fn current_role(
        &self,
        connection: &mut SqliteConnection,
        member: &MemberId,
    ) -> Result<Option<WorkspaceRole>, Fault> {
        sqlx::query_scalar::<_, String>(
            "SELECT role FROM members
             WHERE workspace_id = ? AND member_id = ? AND account_id IS NOT NULL",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .fetch_optional(connection)
        .await
        .map_err(failed)?
        .map(|role| stored_role(&role))
        .transpose()
    }

    /// Ends `member`'s membership for whoever has it, keeping what they did here.
    async fn end_membership(
        &self,
        connection: &mut SqliteConnection,
        member: &MemberId,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE members SET account_id = NULL WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(())
    }
}

fn not_allowed() -> Fault {
    Fault::new(code!("member.not_allowed"), "The member may not do that")
}

fn not_found() -> Fault {
    Fault::new(
        code!("member.not_found"),
        "There is no such member or invitation",
    )
}

fn member_exists() -> Fault {
    Fault::new(code!("member.exists"), "The address is a member already")
}

fn invitation_exists() -> Fault {
    Fault::new(code!("invitation.exists"), "The address is invited already")
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AccountId, WorkspaceChange};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, code, id};
    use crate::db::{Db, NewWorkspace};
    use crate::test_support::{email, fresh_account, member_id, workspace_id};

    // A database where ada owns w1 as o1 and w2 as o2, and bo is a member of w1 as m2; and w1.
    async fn team(dir: &TempDir) -> (Db, Workspace) {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let ada: AccountId = id("a-ada");
        db.add_account(&ada, &email("ada@example.com"))
            .await
            .unwrap();
        for (workspace, owner) in [("w1", "o1"), ("w2", "o2")] {
            let new = NewWorkspace {
                id: workspace_id(workspace),
                name: workspace.to_uppercase(),
                owner: member_id(owner),
            };
            db.account(ada.clone()).create_workspace(new).await.unwrap();
        }
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();
        let bo = email("bo@example.com");
        w1.add_member(&member_id("m2"), &fresh_account(), &bo)
            .await
            .unwrap();
        (db, w1)
    }

    async fn invite(workspace: &Workspace, by: &str, to: &str, now: i64) -> Result<(), Fault> {
        workspace
            .invite(&member_id(by), &fresh_account(), &email(to), now)
            .await
    }

    async fn changes(workspace: &Workspace) -> usize {
        workspace
            .changes_since(0, 500)
            .await
            .unwrap()
            .into_iter()
            .filter(|update| update.change == WorkspaceChange::MembersChanged)
            .count()
    }

    fn listed(rows: &[InvitationRow]) -> Vec<&str> {
        rows.iter().map(|row| row.email.as_str()).collect()
    }

    #[tokio::test]
    async fn only_the_owner_invites_and_an_invitation_is_listed_to_its_workspace_alone() {
        let dir = TempDir::new().unwrap();
        let (db, w1) = team(&dir).await;
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();

        let by_member = invite(&w1, "m2", "cy@example.com", NOW).await.unwrap_err();
        let not_here = invite(&w2, "o1", "cy@example.com", NOW).await.unwrap_err();
        invite(&w1, "o1", "cy@example.com", NOW).await.unwrap();
        invite(&w1, "o1", "di@example.com", NOW + 1).await.unwrap();

        assert_eq!(code(&by_member), "member.not_allowed");
        assert_eq!(code(&not_here), "member.not_allowed");
        let live = w1.invitations(NOW).await.unwrap();
        assert_eq!(listed(&live), ["cy@example.com", "di@example.com"]);
        assert_eq!(live[0].expires_at, NOW + INVITATION_LIFETIME);
        assert!(w2.invitations(NOW).await.unwrap().is_empty());
        let ended = NOW + 1 + INVITATION_LIFETIME;
        assert!(w1.invitations(ended).await.unwrap().is_empty());
        // An invitation is no member, past or present.
        let members: Vec<_> = w1.members().await.unwrap();
        let members: Vec<_> = members.into_iter().map(|row| row.id).collect();
        assert_eq!(members, [member_id("o1"), member_id("m2")]);
        assert_eq!((changes(&w1).await, changes(&w2).await), (2, 0));
    }

    #[tokio::test]
    async fn an_address_has_one_live_invitation_and_a_member_none() {
        let dir = TempDir::new().unwrap();
        let (_db, w1) = team(&dir).await;
        let (o1, cy) = (member_id("o1"), email("cy@example.com"));

        let bo = invite(&w1, "o1", "bo@example.com", NOW).await.unwrap_err();
        invite(&w1, "o1", "cy@example.com", NOW).await.unwrap();
        let again = invite(&w1, "o1", "cy@example.com", NOW + 1)
            .await
            .unwrap_err();
        let by_member = w1.revoke(&member_id("m2"), &cy, NOW + 2).await.unwrap_err();
        w1.revoke(&o1, &cy, NOW + 2).await.unwrap();
        let revoked = w1.revoke(&o1, &cy, NOW + 2).await.unwrap_err();
        invite(&w1, "o1", "cy@example.com", NOW + 3).await.unwrap();

        assert_eq!(code(&bo), "member.exists");
        assert_eq!(code(&again), "invitation.exists");
        assert_eq!(code(&by_member), "member.not_allowed");
        assert_eq!(code(&revoked), "member.not_found");
        let live = w1.invitations(NOW + 3).await.unwrap();
        assert_eq!(listed(&live), ["cy@example.com"]);
        assert_eq!(live[0].expires_at, NOW + 3 + INVITATION_LIFETIME);
    }

    #[tokio::test]
    async fn the_owner_removes_members_members_leave_and_the_owner_stays() {
        let dir = TempDir::new().unwrap();
        let (db, w1) = team(&dir).await;
        let cy = email("cy@example.com");
        w1.add_member(&member_id("m3"), &fresh_account(), &cy)
            .await
            .unwrap();
        let (o1, m2, m3) = (member_id("o1"), member_id("m2"), member_id("m3"));

        let member_removes = w1.remove(&m2, &m3).await.unwrap_err();
        let removes_owner = w1.remove(&o1, &o1).await.unwrap_err();
        w1.remove(&o1, &m3).await.unwrap();
        let gone = w1.remove(&o1, &m3).await.unwrap_err();
        let removed_leaves = w1.leave(&m3).await.unwrap_err();
        let owner_leaves = w1.leave(&o1).await.unwrap_err();
        w1.leave(&m2).await.unwrap();

        let faults = [
            member_removes,
            removes_owner,
            gone,
            removed_leaves,
            owner_leaves,
        ];
        let codes: Vec<_> = faults.iter().map(code).collect();
        let (refused, missing) = ("member.not_allowed", "member.not_found");
        assert_eq!(codes, [refused, missing, missing, missing, refused]);
        let roles: Vec<_> = w1.members().await.unwrap();
        let roles: Vec<_> = roles.into_iter().map(|row| row.role).collect();
        assert_eq!(roles, [Some(WorkspaceRole::Owner), None, None]);
        assert_eq!(changes(&w1).await, 2);
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        assert_eq!(changes(&w2).await, 0);
    }

    #[tokio::test]
    async fn a_member_who_leaves_is_nobody_to_their_machines_there_only() {
        let dir = TempDir::new().unwrap();
        let (one, two) = crate::test_support::two_teams(&dir).await;
        let ada = one.db.account_with(&email("ada@example.com")).await;
        let ada = ada.unwrap().unwrap();
        one.pair_for("m1", "mc-1").await;
        crate::db::test_support::make(&one, "c-1", "m1", NOW)
            .await
            .unwrap();
        crate::db::test_support::begin(&one, "c-2", "m1", NOW)
            .await
            .unwrap();
        let machine: weacon_protocol::MachineId = id("mc-1");
        let chat: weacon_protocol::ChatId = id("c-1");
        assert!(one.leg_on(&chat, &machine).await.unwrap().is_some());
        assert_eq!(one.legs_on(&machine, 9).await.unwrap().len(), 1);
        assert!(one.machine_member(&ada, &machine).await.unwrap().is_some());

        one.leave(&member_id("m1")).await.unwrap();

        assert_eq!(one.leg_on(&chat, &machine).await.unwrap(), None);
        assert!(one.legs_on(&machine, 9).await.unwrap().is_empty());
        assert_eq!(one.machine_member(&ada, &machine).await.unwrap(), None);
        let theirs = two.machine_member(&ada, &machine).await.unwrap();
        assert_eq!(theirs.map(|(member, _)| member), Some(member_id("m2")));
    }

    #[test]
    fn every_role_this_server_writes_reads_back_and_no_other() {
        assert_eq!(stored_role("owner").unwrap(), WorkspaceRole::Owner);
        assert_eq!(stored_role("member").unwrap(), WorkspaceRole::Member);
        assert_eq!(code(&stored_role("admin").unwrap_err()), "db.unreadable");
    }
}
