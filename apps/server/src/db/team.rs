//! Who is in a workspace: roles, invitations, and memberships that end.

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{MemberId, WorkspaceRole, code};

use super::changes::Versioned;
use super::{Workspace, failed, stored, unreadable};
use crate::email::Email;

mod offers;

pub(super) const OWNER: &str = "owner";
const ADMIN: &str = "admin";
pub(super) const MEMBER: &str = "member";
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;
/// How long an invitation lasts, in seconds.
pub(crate) const INVITATION_LIFETIME: i64 = 7 * DAY;
/// How many live invitations a workspace may have.
const MAX_LIVE: i64 = 50;
/// How many invitations one account may make in an hour, across every workspace.
const MAX_PER_HOUR: i64 = 20;
/// How many notices one address may get in a day, across the server.
const MAX_NOTICES: i64 = 3;

/// An invitation to make: `member` is the id it gets if its address has never been in the
/// workspace. Times are seconds since the Unix epoch.
pub(crate) struct NewInvitation<'a> {
    pub(crate) member: &'a MemberId,
    pub(crate) by: &'a MemberId,
    pub(crate) email: &'a Email,
    pub(crate) role: WorkspaceRole,
    pub(crate) now: i64,
}

/// A live invitation, as the workspace's owner and admins see it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvitationRow {
    pub(crate) member: MemberId,
    pub(crate) email: Email,
    pub(crate) role: WorkspaceRole,
    pub(crate) expires_at: i64,
}

pub(super) const fn role_name(role: WorkspaceRole) -> Option<&'static str> {
    match role {
        WorkspaceRole::Owner => Some(OWNER),
        WorkspaceRole::Admin => Some(ADMIN),
        WorkspaceRole::Member => Some(MEMBER),
        WorkspaceRole::Newer => None,
    }
}

pub(super) fn stored_role(name: &str) -> Result<WorkspaceRole, Fault> {
    match name {
        OWNER => Ok(WorkspaceRole::Owner),
        ADMIN => Ok(WorkspaceRole::Admin),
        MEMBER => Ok(WorkspaceRole::Member),
        // This server writes every role it stores, so another is damage, not news.
        _ => Err(unreadable()),
    }
}

/// Whether someone who is `by` may give `role` to, or take it from, someone else: the owner
/// to admins and members, an admin to members.
const fn may_manage(by: WorkspaceRole, role: WorkspaceRole) -> bool {
    matches!(
        (by, role),
        (
            WorkspaceRole::Owner,
            WorkspaceRole::Admin | WorkspaceRole::Member
        ) | (WorkspaceRole::Admin, WorkspaceRole::Member)
    )
}

impl Workspace {
    /// Invites `new.email`, and says whether to tell the address: not when it was told of this
    /// workspace in the last day, nor when it has had [`MAX_NOTICES`] notices in a day.
    pub(crate) async fn invite(&self, new: &NewInvitation<'_>) -> Result<bool, Fault> {
        let mut transaction = self.db.write().await?;
        let by = self.current_role(&mut transaction, new.by).await?;
        if !by.is_some_and(|by| may_manage(by, new.role)) {
            return Err(not_allowed());
        }
        let (had, noticed) = self.earlier(&mut transaction, new).await?;
        self.check_room(&mut transaction, new).await?;
        let member = self.store_invitation(&mut transaction, had, new).await?;
        let tell = !noticed && self.notice(&mut transaction, &member, new).await?;
        self.record_change(&mut transaction, Versioned::Members, new.by)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(tell)
    }

    /// The member `new.email` had here, if any, and whether it was told of an invitation in the
    /// last day; a member now, or one with a live invitation, can't be invited.
    async fn earlier(
        &self,
        transaction: &mut SqliteConnection,
        new: &NewInvitation<'_>,
    ) -> Result<(Option<MemberId>, bool), Fault> {
        let found = sqlx::query_as::<_, (String, Option<String>, Option<i64>, Option<i64>)>(
            "SELECT m.member_id, m.account_id, i.expires_at, i.noticed_at FROM members AS m
             LEFT JOIN invitations AS i USING (workspace_id, member_id)
             WHERE m.workspace_id = ? AND m.email = ?",
        )
        .bind(self.id.as_str())
        .bind(new.email.as_str())
        .fetch_optional(transaction)
        .await
        .map_err(failed)?;
        match found {
            None => Ok((None, false)),
            Some((_, Some(_), _, _)) => Err(member_exists()),
            Some((_, None, Some(ends), _)) if ends > new.now => Err(invitation_exists()),
            Some((member, None, _, noticed)) => {
                let recent = noticed.is_some_and(|at| at > new.now - DAY);
                Ok((Some(stored(member)?), recent))
            }
        }
    }

    /// Stores the invitation, on the member `had` or a new one, and gives the member.
    async fn store_invitation(
        &self,
        transaction: &mut SqliteConnection,
        had: Option<MemberId>,
        new: &NewInvitation<'_>,
    ) -> Result<MemberId, Fault> {
        let role = role_name(new.role).ok_or_else(not_allowed)?;
        let member = had.unwrap_or_else(|| new.member.clone());
        sqlx::query(
            "INSERT INTO members (workspace_id, member_id, email, role, joined)
             VALUES (?, ?, ?, ?, 0)
             ON CONFLICT (workspace_id, member_id) DO UPDATE SET role = excluded.role",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .bind(new.email.as_str())
        .bind(role)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        sqlx::query(
            "INSERT INTO invitations (workspace_id, member_id, invited_by, invited_at, expires_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (workspace_id, member_id) DO UPDATE SET invited_by = excluded.invited_by,
                 invited_at = excluded.invited_at, expires_at = excluded.expires_at, hidden = 0",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .bind(new.by.as_str())
        .bind(new.now)
        .bind(new.now.saturating_add(INVITATION_LIFETIME))
        .execute(transaction)
        .await
        .map_err(failed)?;
        Ok(member)
    }

    /// Refuses an invitation past the workspace's or its maker's limits.
    async fn check_room(
        &self,
        transaction: &mut SqliteConnection,
        new: &NewInvitation<'_>,
    ) -> Result<(), Fault> {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM invitations WHERE workspace_id = ? AND expires_at > ?",
        )
        .bind(self.id.as_str())
        .bind(new.now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        let made: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM invitations AS i
             JOIN members AS b ON b.workspace_id = i.workspace_id AND b.member_id = i.invited_by
             WHERE b.account_id = (SELECT account_id FROM members
                     WHERE workspace_id = ? AND member_id = ?)
                 AND i.invited_at > ?",
        )
        .bind(self.id.as_str())
        .bind(new.by.as_str())
        .bind(new.now - HOUR)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        if live >= MAX_LIVE || made >= MAX_PER_HOUR {
            return Err(too_many());
        }
        Ok(())
    }

    /// Notes that `member`'s address is told of its invitation, unless it had its share today.
    async fn notice(
        &self,
        transaction: &mut SqliteConnection,
        member: &MemberId,
        new: &NewInvitation<'_>,
    ) -> Result<bool, Fault> {
        let told: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM invitations AS i JOIN members AS m USING (workspace_id, member_id)
             WHERE m.email = ? AND i.noticed_at > ?",
        )
        .bind(new.email.as_str())
        .bind(new.now - DAY)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        if told >= MAX_NOTICES {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE invitations SET noticed_at = ? WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(new.now)
        .bind(self.id.as_str())
        .bind(member.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(true)
    }

    /// Ends `member`'s live invitation, if `by` is the owner or an admin.
    pub(crate) async fn revoke(
        &self,
        by: &MemberId,
        member: &MemberId,
        now: i64,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        let by_role = self.current_role(&mut transaction, by).await?;
        if !by_role.is_some_and(|by| may_manage(by, WorkspaceRole::Member)) {
            return Err(not_allowed());
        }
        let ended = sqlx::query(
            "UPDATE invitations SET expires_at = 0
             WHERE workspace_id = ? AND member_id = ? AND expires_at > ?",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
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

    /// Ends `member`'s membership, if `by` may take their role from them.
    pub(crate) async fn remove(&self, by: &MemberId, member: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        let by_role = self.current_role(&mut transaction, by).await?;
        let role = self.current_role(&mut transaction, member).await?;
        let Some(role) = role else {
            return Err(not_found());
        };
        if !by_role.is_some_and(|by| may_manage(by, role)) {
            return Err(not_allowed());
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
            Some(WorkspaceRole::Owner) => return Err(not_allowed()),
            Some(_) => {}
        }
        self.end_membership(&mut transaction, member).await?;
        self.record_change(&mut transaction, Versioned::Members, member)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Gives `member` `role`, admin or member; only the owner may, and not to themselves.
    pub(crate) async fn set_role(
        &self,
        by: &MemberId,
        member: &MemberId,
        role: WorkspaceRole,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        let by_role = self.current_role(&mut transaction, by).await?;
        let Some(had) = self.current_role(&mut transaction, member).await? else {
            return Err(not_found());
        };
        let owner = by_role == Some(WorkspaceRole::Owner);
        if !owner
            || !may_manage(WorkspaceRole::Owner, had)
            || !may_manage(WorkspaceRole::Owner, role)
        {
            return Err(not_allowed());
        }
        sqlx::query("UPDATE members SET role = ? WHERE workspace_id = ? AND member_id = ?")
            .bind(role_name(role).ok_or_else(not_allowed)?)
            .bind(self.id.as_str())
            .bind(member.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.record_change(&mut transaction, Versioned::Members, by)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// The live invitations, oldest first.
    pub(crate) async fn invitations(&self, now: i64) -> Result<Vec<InvitationRow>, Fault> {
        sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT m.member_id, m.email, m.role, i.expires_at
             FROM invitations AS i JOIN members AS m USING (workspace_id, member_id)
             WHERE i.workspace_id = ? AND i.expires_at > ? ORDER BY i.invited_at, m.rowid",
        )
        .bind(self.id.as_str())
        .bind(now)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(member, email, role, expires_at)| {
            Ok(InvitationRow {
                member: stored(member)?,
                email: Email::parse(&email).map_err(|_| unreadable())?,
                role: stored_role(&role)?,
                expires_at,
            })
        })
        .collect()
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

fn too_many() -> Fault {
    Fault::new(code!("invitation.too_many"), "Too many invitations for now")
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
        w1.add_member(&member_id("m2"), &fresh_account(), &email("bo@example.com"))
            .await
            .unwrap();
        (db, w1)
    }

    fn invitation<'a>(member: &'a MemberId, by: &'a MemberId, to: &'a Email) -> NewInvitation<'a> {
        NewInvitation {
            member,
            by,
            email: to,
            role: WorkspaceRole::Member,
            now: NOW,
        }
    }

    // Invites `to` into `workspace` as `role`, by `by`, at `now`, as the new member `member`.
    async fn invite(
        workspace: &Workspace,
        by: &str,
        to: &str,
        role: WorkspaceRole,
        now: i64,
    ) -> Result<bool, Fault> {
        let (member, by, to) = (
            member_id(&format!("i-{}", to.split('@').next().unwrap())),
            member_id(by),
            email(to),
        );
        let new = NewInvitation {
            role,
            now,
            ..invitation(&member, &by, &to)
        };
        workspace.invite(&new).await
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

    fn listed(rows: &[InvitationRow]) -> Vec<(&str, &str, WorkspaceRole)> {
        rows.iter()
            .map(|row| (row.member.as_str(), row.email.as_str(), row.role))
            .collect()
    }

    #[tokio::test]
    async fn the_owner_invites_admins_and_members_and_admins_only_members() {
        let dir = TempDir::new().unwrap();
        let (db, w1) = team(&dir).await;
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        let refused = async |workspace: &Workspace, by: &str, to: &str, role| {
            let fault = invite(workspace, by, to, role, NOW).await.unwrap_err();
            code(&fault).to_owned()
        };

        assert_eq!(
            refused(&w1, "m2", "cy@example.com", WorkspaceRole::Member).await,
            "member.not_allowed"
        );
        assert!(
            invite(&w1, "o1", "cy@example.com", WorkspaceRole::Admin, NOW)
                .await
                .unwrap()
        );
        w1.set_role(&member_id("o1"), &member_id("m2"), WorkspaceRole::Admin)
            .await
            .unwrap();
        assert_eq!(
            refused(&w1, "m2", "di@example.com", WorkspaceRole::Admin).await,
            "member.not_allowed"
        );
        assert_eq!(
            refused(&w1, "m2", "ed@example.com", WorkspaceRole::Owner).await,
            "member.not_allowed"
        );
        assert!(
            invite(&w1, "m2", "di@example.com", WorkspaceRole::Member, NOW)
                .await
                .unwrap()
        );
        assert_eq!(
            refused(&w2, "o1", "fe@example.com", WorkspaceRole::Member).await,
            "member.not_allowed"
        );
        assert_eq!(changes(&w1).await, 3);
        assert_eq!(changes(&w2).await, 0);
    }

    #[tokio::test]
    async fn an_invitation_is_listed_to_its_workspace_alone_and_is_no_member() {
        let dir = TempDir::new().unwrap();
        let (db, w1) = team(&dir).await;
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        invite(&w1, "o1", "cy@example.com", WorkspaceRole::Admin, NOW)
            .await
            .unwrap();
        invite(&w1, "o1", "di@example.com", WorkspaceRole::Member, NOW)
            .await
            .unwrap();

        let live = w1.invitations(NOW).await.unwrap();
        assert_eq!(
            listed(&live),
            [
                ("i-cy", "cy@example.com", WorkspaceRole::Admin),
                ("i-di", "di@example.com", WorkspaceRole::Member)
            ]
        );
        assert_eq!(live[0].expires_at, NOW + INVITATION_LIFETIME);
        assert!(w2.invitations(NOW).await.unwrap().is_empty());
        assert!(
            w1.invitations(NOW + INVITATION_LIFETIME)
                .await
                .unwrap()
                .is_empty()
        );
        let members: Vec<_> = w1
            .members()
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(members, [member_id("o1"), member_id("m2")]);
    }

    #[tokio::test]
    async fn an_address_has_one_live_invitation_and_a_member_none() {
        let dir = TempDir::new().unwrap();
        let (_db, w1) = team(&dir).await;
        let member = WorkspaceRole::Member;

        let bo = invite(&w1, "o1", "bo@example.com", member, NOW).await;
        assert_eq!(code(&bo.unwrap_err()), "member.exists");
        invite(&w1, "o1", "cy@example.com", member, NOW)
            .await
            .unwrap();
        let again = invite(&w1, "o1", "cy@example.com", member, NOW + 1).await;
        assert_eq!(code(&again.unwrap_err()), "invitation.exists");
        w1.revoke(&member_id("o1"), &member_id("i-cy"), NOW + 2)
            .await
            .unwrap();
        assert!(w1.invitations(NOW + 2).await.unwrap().is_empty());
        let revoked = w1
            .revoke(&member_id("o1"), &member_id("i-cy"), NOW + 2)
            .await;
        assert_eq!(code(&revoked.unwrap_err()), "member.not_found");
        // Asked again, the address keeps its member, and its notice of today.
        let admin = WorkspaceRole::Admin;
        assert!(
            !invite(&w1, "o1", "cy@example.com", admin, NOW + 3)
                .await
                .unwrap()
        );
        let expired = NOW + 3 + INVITATION_LIFETIME;
        assert!(
            invite(&w1, "o1", "cy@example.com", member, expired)
                .await
                .unwrap()
        );
        assert_eq!(
            listed(&w1.invitations(expired).await.unwrap()),
            [("i-cy", "cy@example.com", member)]
        );
        let by_member = w1
            .revoke(&member_id("m2"), &member_id("i-cy"), expired)
            .await;
        assert_eq!(code(&by_member.unwrap_err()), "member.not_allowed");
    }

    // `team`, with cy as m3 and di as m4 besides.
    async fn bigger(dir: &TempDir) -> (Db, Workspace) {
        let (db, w1) = team(dir).await;
        for (member, address) in [("m3", "cy@example.com"), ("m4", "di@example.com")] {
            w1.add_member(&member_id(member), &fresh_account(), &email(address))
                .await
                .unwrap();
        }
        (db, w1)
    }

    #[tokio::test]
    async fn only_the_owner_sets_roles_and_never_its_own() {
        let dir = TempDir::new().unwrap();
        let (_db, w1) = bigger(&dir).await;
        let (o1, m2, m3) = (member_id("o1"), member_id("m2"), member_id("m3"));
        let admin = WorkspaceRole::Admin;

        let by_member = w1.set_role(&m2, &m3, admin).await.unwrap_err();
        let to_owner = w1
            .set_role(&o1, &m3, WorkspaceRole::Owner)
            .await
            .unwrap_err();
        let of_owner = w1.set_role(&o1, &o1, admin).await.unwrap_err();
        let nobody = w1.set_role(&o1, &member_id("m9"), admin).await.unwrap_err();
        w1.set_role(&o1, &m2, admin).await.unwrap();
        w1.set_role(&o1, &m3, admin).await.unwrap();
        let admin_on_admin = w1
            .set_role(&m2, &m3, WorkspaceRole::Member)
            .await
            .unwrap_err();

        let codes: Vec<_> = [by_member, to_owner, of_owner, nobody, admin_on_admin]
            .iter()
            .map(|fault| code(fault).to_owned())
            .collect();
        assert_eq!(
            codes,
            [
                "member.not_allowed",
                "member.not_allowed",
                "member.not_allowed",
                "member.not_found",
                "member.not_allowed"
            ]
        );
        assert_eq!(changes(&w1).await, 2);
    }

    #[tokio::test]
    async fn the_owner_removes_admins_and_members_admins_only_members_and_the_owner_stays() {
        let dir = TempDir::new().unwrap();
        let (db, w1) = bigger(&dir).await;
        let (o1, m2, m3, m4) = (
            member_id("o1"),
            member_id("m2"),
            member_id("m3"),
            member_id("m4"),
        );
        for member in [&m2, &m3] {
            w1.set_role(&o1, member, WorkspaceRole::Admin)
                .await
                .unwrap();
        }

        let removes_admin = w1.remove(&m2, &m3).await.unwrap_err();
        let removes_owner = w1.remove(&m2, &o1).await.unwrap_err();
        w1.remove(&m2, &m4).await.unwrap();
        let gone = w1.remove(&m2, &m4).await.unwrap_err();
        let removed_acts = w1.leave(&m4).await.unwrap_err();
        w1.remove(&o1, &m3).await.unwrap();
        let owner_leaves = w1.leave(&o1).await.unwrap_err();
        w1.leave(&m2).await.unwrap();

        let faults = [
            removes_admin,
            removes_owner,
            gone,
            removed_acts,
            owner_leaves,
        ];
        let (refused, missing) = ("member.not_allowed", "member.not_found");
        let codes: Vec<_> = faults.iter().map(code).collect();
        assert_eq!(codes, [refused, refused, missing, missing, refused]);
        let roles: Vec<_> = w1
            .members()
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.role)
            .collect();
        assert_eq!(roles, [Some(WorkspaceRole::Owner), None, None, None]);
        assert_eq!(changes(&w1).await, 5);
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        assert_eq!(changes(&w2).await, 0);
    }

    #[tokio::test]
    async fn a_workspace_and_an_inviter_make_only_so_many_invitations() {
        let dir = TempDir::new().unwrap();
        let (db, w1) = team(&dir).await;
        let w2 = db.workspace(&workspace_id("w2")).await.unwrap();
        let member = WorkspaceRole::Member;
        for index in 0..MAX_PER_HOUR - 1 {
            invite(&w1, "o1", &format!("p{index}@example.com"), member, NOW)
                .await
                .unwrap();
        }
        invite(&w2, "o2", "q@example.com", member, NOW)
            .await
            .unwrap();

        let hourly = invite(&w1, "o1", "r@example.com", member, NOW).await;
        let elsewhere = invite(&w2, "o2", "r@example.com", member, NOW + HOUR - 1).await;
        assert_eq!(code(&hourly.unwrap_err()), "invitation.too_many");
        assert_eq!(code(&elsewhere.unwrap_err()), "invitation.too_many");
        invite(&w1, "o1", "r@example.com", member, NOW + HOUR)
            .await
            .unwrap();
        let now = NOW + 2 * HOUR;
        for index in 0..MAX_LIVE - MAX_PER_HOUR {
            let at = now + HOUR * (index / 10);
            invite(&w1, "o1", &format!("s{index}@example.com"), member, at)
                .await
                .unwrap();
        }
        let later = now + 4 * HOUR;
        let full = invite(&w1, "o1", "t@example.com", member, later).await;
        assert_eq!(code(&full.unwrap_err()), "invitation.too_many");
        assert_eq!(
            i64::try_from(w1.invitations(later).await.unwrap().len()).unwrap(),
            MAX_LIVE
        );
        w1.revoke(&member_id("o1"), &member_id("i-t0"), later)
            .await
            .unwrap_err();
        w1.revoke(&member_id("o1"), &member_id("i-p0"), later)
            .await
            .unwrap();
        invite(&w1, "o1", "t@example.com", member, later)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_address_is_told_of_three_invitations_a_day_at_most() {
        let dir = TempDir::new().unwrap();
        let (db, _) = team(&dir).await;
        let ada: AccountId = id("a-ada");
        let mut told = Vec::new();
        for index in 0..5 {
            let workspace = format!("x{index}");
            let new = NewWorkspace {
                id: workspace_id(&workspace),
                name: "X".to_owned(),
                owner: member_id(&format!("ox{index}")),
            };
            let place = db.account(ada.clone()).create_workspace(new).await.unwrap();
            let place = db.workspace(&place.id).await.unwrap();
            let by = format!("ox{index}");
            let at = NOW + index;
            told.push(invite(&place, &by, "cy@example.com", WorkspaceRole::Member, at).await);
        }
        let x0 = db.workspace(&workspace_id("x0")).await.unwrap();
        let tomorrow = NOW + DAY;
        x0.revoke(&member_id("ox0"), &member_id("i-cy"), tomorrow)
            .await
            .unwrap();
        let again = invite(
            &x0,
            "ox0",
            "cy@example.com",
            WorkspaceRole::Member,
            tomorrow + 1,
        );

        let told: Vec<_> = told.into_iter().map(Result::unwrap).collect();
        assert_eq!(told, [true, true, true, false, false]);
        assert!(again.await.unwrap());
    }

    #[tokio::test]
    async fn a_member_who_leaves_is_nobody_to_their_machines_there_only() {
        let dir = TempDir::new().unwrap();
        let (one, two) = crate::test_support::two_teams(&dir).await;
        let ada = one
            .db
            .account_with(&email("ada@example.com"))
            .await
            .unwrap()
            .unwrap();
        one.pair_for("m1", "mc-1").await;
        crate::db::test_support::make(&one, "c-1", "m1", NOW)
            .await
            .unwrap();
        crate::db::test_support::begin(&one, "c-2", "m1", NOW)
            .await
            .unwrap();
        let machine: weacon_protocol::MachineId = id("mc-1");
        let chat: weacon_protocol::ChatId = id("c-1");
        assert!(one.runs_on(&chat, &machine).await.unwrap());
        assert_eq!(one.unmade_on(&machine).await.unwrap().len(), 1);
        assert!(one.machine_member(&ada, &machine).await.unwrap().is_some());

        one.leave(&member_id("m1")).await.unwrap();

        assert!(!one.runs_on(&chat, &machine).await.unwrap());
        assert!(one.unmade_on(&machine).await.unwrap().is_empty());
        assert!(one.unsettled_on(&machine).await.unwrap().is_empty());
        assert_eq!(one.machine_member(&ada, &machine).await.unwrap(), None);
        let theirs = two.machine_member(&ada, &machine).await.unwrap();
        assert_eq!(theirs.map(|(member, _)| member), Some(member_id("m2")));
    }

    #[test]
    fn every_role_this_server_writes_reads_back_and_no_other() {
        for role in [
            WorkspaceRole::Owner,
            WorkspaceRole::Admin,
            WorkspaceRole::Member,
        ] {
            assert_eq!(stored_role(role_name(role).unwrap()).unwrap(), role);
        }
        assert_eq!(role_name(WorkspaceRole::Newer), None);
        assert_eq!(code(&stored_role("auditor").unwrap_err()), "db.unreadable");
    }
}
