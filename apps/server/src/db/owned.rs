use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, MemberId, WorkspaceId, WorkspaceRole, WorkspaceSummary};

use super::{Db, Workspace, failed, stored, unreadable};

const OWNER: &str = "owner";
const MEMBER: &str = "member";
/// How many workspaces one account may own: enough for anyone, too few to fill the disk.
const MAX_OWNED: i64 = 100;

/// A workspace to make, and the id its first member, who owns it, gets there.
pub(crate) struct NewWorkspace {
    pub(crate) id: WorkspaceId,
    pub(crate) name: String,
    pub(crate) owner: MemberId,
}

impl Db {
    /// Makes a workspace that `account` owns, as its first member; `None` when it owns as
    /// many as it may.
    pub(crate) async fn create_owned_workspace(
        &self,
        account: &AccountId,
        new: NewWorkspace,
    ) -> Result<Option<WorkspaceSummary>, Fault> {
        let mut transaction = self
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let owned: i64 =
            sqlx::query_scalar("SELECT count(*) FROM members WHERE account_id = ? AND role = ?")
                .bind(account.as_str())
                .bind(OWNER)
                .fetch_one(&mut *transaction)
                .await
                .map_err(failed)?;
        if owned >= MAX_OWNED {
            return Ok(None);
        }
        insert_owned(&mut transaction, account, &new).await?;
        transaction.commit().await.map_err(failed)?;
        Ok(Some(WorkspaceSummary {
            id: new.id,
            name: new.name,
            role: WorkspaceRole::Owner,
        }))
    }

    /// The workspaces `account` is a member of, oldest membership first.
    pub(crate) async fn workspaces_of(
        &self,
        account: &AccountId,
    ) -> Result<Vec<WorkspaceSummary>, Fault> {
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT w.workspace_id, w.name, m.role
             FROM members AS m JOIN workspaces AS w ON w.workspace_id = m.workspace_id
             WHERE m.account_id = ? ORDER BY m.rowid",
        )
        .bind(account.as_str())
        .fetch_all(&self.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, name, role)| summary(stored(id)?, name, &role))
        .collect()
    }
}

impl Workspace {
    /// The workspace as `member` sees it, if they are still one.
    pub(crate) async fn summary(
        &self,
        member: &MemberId,
    ) -> Result<Option<WorkspaceSummary>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT w.name, m.role
             FROM members AS m JOIN workspaces AS w ON w.workspace_id = m.workspace_id
             WHERE m.workspace_id = ? AND m.member_id = ?",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(|(name, role)| summary(self.id.clone(), name, &role))
        .transpose()
    }
}

/// Makes `account`'s Personal workspace as `new`, unless it has had one.
pub(super) async fn ensure_personal(
    transaction: &mut SqliteConnection,
    account: &str,
    new: &NewWorkspace,
) -> Result<(), Fault> {
    let unmade = sqlx::query(
        "UPDATE accounts SET personal_made = 1 WHERE account_id = ? AND personal_made = 0",
    )
    .bind(account)
    .execute(&mut *transaction)
    .await
    .map_err(failed)?;
    if unmade.rows_affected() == 0 {
        return Ok(());
    }
    insert_owned(transaction, &stored(account.to_owned())?, new).await
}

async fn insert_owned(
    transaction: &mut SqliteConnection,
    account: &AccountId,
    new: &NewWorkspace,
) -> Result<(), Fault> {
    super::insert_workspace(&mut *transaction, &new.id, &new.name).await?;
    let added = sqlx::query(
        "INSERT INTO members (workspace_id, member_id, email, account_id, role)
         SELECT ?, ?, email, account_id, ? FROM accounts WHERE account_id = ?",
    )
    .bind(new.id.as_str())
    .bind(new.owner.as_str())
    .bind(OWNER)
    .bind(account.as_str())
    .execute(&mut *transaction)
    .await
    .map_err(failed)?;
    if added.rows_affected() == 1 {
        Ok(())
    } else {
        Err(unreadable())
    }
}

fn summary(id: WorkspaceId, name: String, role: &str) -> Result<WorkspaceSummary, Fault> {
    let role = match role {
        OWNER => WorkspaceRole::Owner,
        MEMBER => WorkspaceRole::Member,
        // This server writes every role it stores, so another is damage, not news.
        _ => return Err(unreadable()),
    };
    Ok(WorkspaceSummary { id, name, role })
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::email::Email;
    use crate::test_support::{member_id, two_teams, workspace_id};

    fn account(id: &str) -> AccountId {
        AccountId::try_from(id.to_owned()).unwrap()
    }

    fn new(id: &str, name: &str) -> NewWorkspace {
        NewWorkspace {
            id: workspace_id(id),
            name: name.to_owned(),
            owner: member_id(&format!("o-{id}")),
        }
    }

    fn seen(id: &str, name: &str, role: WorkspaceRole) -> WorkspaceSummary {
        WorkspaceSummary {
            id: workspace_id(id),
            name: name.to_owned(),
            role,
        }
    }

    // A database with ada, who is in w1 and w2, and cy, who is in none, as a2; and ada's
    // account.
    async fn people(dir: &TempDir) -> (Db, AccountId) {
        let (one, _) = two_teams(dir).await;
        let db = one.db.clone();
        let ada = db
            .account_with(&Email::parse("ada@example.com").unwrap())
            .await;
        db.add_account(&account("a2"), &Email::parse("cy@example.com").unwrap())
            .await
            .unwrap();
        (db, ada.unwrap().unwrap())
    }

    #[tokio::test]
    async fn an_account_owns_what_it_makes_and_sees_only_its_own_workspaces() {
        let dir = TempDir::new().unwrap();
        let (db, ada) = people(&dir).await;

        let made = db
            .create_owned_workspace(&account("a2"), new("w3", "Three"))
            .await;
        let taken = db
            .create_owned_workspace(&account("a2"), new("w3", "Again"))
            .await;
        let nobody = db
            .create_owned_workspace(&account("a9"), new("w4", "Four"))
            .await;

        assert_eq!(
            made.unwrap(),
            Some(seen("w3", "Three", WorkspaceRole::Owner))
        );
        assert_eq!(taken.unwrap_err().code().as_str(), "db.exists");
        assert_eq!(nobody.unwrap_err().code().as_str(), "db.unreadable");
        assert!(
            db.find_workspace(&workspace_id("w4"))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.workspaces_of(&account("a2")).await.unwrap(),
            [seen("w3", "Three", WorkspaceRole::Owner)]
        );
        assert_eq!(
            db.workspaces_of(&ada).await.unwrap(),
            [
                seen("w1", "One", WorkspaceRole::Member),
                seen("w2", "Two", WorkspaceRole::Member)
            ]
        );
        assert!(db.workspaces_of(&account("a9")).await.unwrap().is_empty());
        let w3 = db.workspace(&workspace_id("w3")).await.unwrap();
        let owner = w3.summary(&member_id("o-w3")).await.unwrap().unwrap();
        assert_eq!(owner.role, WorkspaceRole::Owner);
        let w1 = db.workspace(&workspace_id("w1")).await.unwrap();
        assert_eq!(
            w1.summary(&member_id("m1")).await.unwrap().unwrap().name,
            "One"
        );
        assert_eq!(w1.summary(&member_id("m2")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_account_owns_only_so_many_workspaces() {
        let dir = TempDir::new().unwrap();
        let (db, _) = people(&dir).await;
        for index in 0..MAX_OWNED {
            let id = format!("w-{index}");
            db.create_owned_workspace(&account("a2"), new(&id, "Many"))
                .await
                .unwrap();
        }

        let more = db
            .create_owned_workspace(&account("a2"), new("w-x", "More"))
            .await;
        let others = db
            .create_owned_workspace(&account("a9"), new("w-y", "Other"))
            .await;

        assert_eq!(more.unwrap(), None);
        assert_eq!(others.unwrap_err().code().as_str(), "db.unreadable");
    }

    #[tokio::test]
    async fn an_account_gets_its_personal_workspace_once() {
        let dir = TempDir::new().unwrap();
        let (db, _) = people(&dir).await;
        let mut connection = db.writer.acquire().await.unwrap();

        for id in ["p1", "p2"] {
            ensure_personal(&mut connection, "a2", &new(id, "Personal"))
                .await
                .unwrap();
        }
        ensure_personal(&mut connection, "a9", &new("p3", "Personal"))
            .await
            .unwrap();
        drop(connection);

        assert_eq!(
            db.workspaces_of(&account("a2")).await.unwrap(),
            [seen("p1", "Personal", WorkspaceRole::Owner)]
        );
        for id in ["p2", "p3"] {
            assert!(
                db.find_workspace(&workspace_id(id))
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn a_role_this_server_never_writes_is_unreadable() {
        let dir = TempDir::new().unwrap();
        let (db, ada) = people(&dir).await;
        sqlx::query("UPDATE members SET role = 'auditor' WHERE member_id = 'm1'")
            .execute(&db.writer)
            .await
            .unwrap();

        let listed = db.workspaces_of(&ada).await.unwrap_err();
        let read = db.workspace(&workspace_id("w1")).await.unwrap();
        let read = read.summary(&member_id("m1")).await.unwrap_err();

        assert_eq!(listed.code().as_str(), "db.unreadable");
        assert_eq!(read.code().as_str(), "db.unreadable");
    }
}
