use weacon_observability::Fault;
use weacon_protocol::{AccountId, MemberId, WorkspaceId};

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

    /// Adds the person with `email` as member `id`, with the account their address has, or
    /// else `new_account`. An id or email already in this workspace is `db.exists`.
    pub(crate) async fn add_member(
        &self,
        id: &MemberId,
        new_account: &AccountId,
        email: &Email,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        sqlx::query(
            "INSERT INTO accounts (account_id, email) VALUES (?, ?)
             ON CONFLICT (email) DO NOTHING",
        )
        .bind(new_account.as_str())
        .bind(email.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        sqlx::query(
            "INSERT INTO members (workspace_id, member_id, email, account_id)
             SELECT ?, ?, email, account_id FROM accounts WHERE email = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .bind(email.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)
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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, raw_sql};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::schema::STEPS;
    use crate::test_support::{fresh_account, member_id, workspace_id};

    fn email(text: &str) -> Email {
        Email::parse(text).unwrap()
    }

    async fn accounts(db: &Db) -> Vec<(String, String)> {
        sqlx::query_as("SELECT account_id, email FROM accounts ORDER BY email")
            .fetch_all(&db.readers)
            .await
            .unwrap()
    }

    async fn member_accounts(db: &Db) -> Vec<(String, String, Option<String>)> {
        sqlx::query_as("SELECT workspace_id, member_id, account_id FROM members ORDER BY rowid")
            .fetch_all(&db.readers)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_member_gets_the_account_of_their_address_or_a_new_one() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let one = db
            .create_workspace(&workspace_id("w1"), "One")
            .await
            .unwrap();
        let two = db
            .create_workspace(&workspace_id("w2"), "Two")
            .await
            .unwrap();
        let (ada, bo, unused) = (fresh_account(), fresh_account(), fresh_account());
        let (ada_email, bo_email) = (email("ada@example.com"), email("bo@example.com"));
        one.add_member(&member_id("m1"), &ada, &ada_email)
            .await
            .unwrap();
        two.add_member(&member_id("m1"), &unused, &ada_email)
            .await
            .unwrap();
        one.add_member(&member_id("m2"), &bo, &bo_email)
            .await
            .unwrap();

        let twice = one.add_member(&member_id("m3"), &unused, &bo_email).await;
        // The id is taken, so neither the member nor an account for the address is kept.
        let taken = one
            .add_member(&member_id("m1"), &unused, &email("cy@example.com"))
            .await;

        assert_eq!(twice.unwrap_err().code().as_str(), "db.exists");
        assert_eq!(taken.unwrap_err().code().as_str(), "db.exists");
        let linked = |workspace: &str, member: &str, account: &AccountId| {
            let account = Some(account.as_str().to_owned());
            (workspace.to_owned(), member.to_owned(), account)
        };
        assert_eq!(
            member_accounts(&db).await,
            [
                linked("w1", "m1", &ada),
                linked("w2", "m1", &ada),
                linked("w1", "m2", &bo)
            ]
        );
        let owner =
            |account: &AccountId, email: &str| (account.as_str().to_owned(), email.to_owned());
        assert_eq!(
            accounts(&db).await,
            [owner(&ada, "ada@example.com"), owner(&bo, "bo@example.com")]
        );
    }

    #[tokio::test]
    async fn members_from_before_accounts_get_one_per_address() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let mut old = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        for step in &STEPS[..3] {
            raw_sql(*step).execute(&mut old).await.unwrap();
        }
        raw_sql(
            "INSERT INTO workspaces VALUES ('w1', 'One'), ('w2', 'Two');
             INSERT INTO members VALUES
                 ('w1', 'm1', 'ada@example.com'), ('w2', 'm1', 'ada@example.com'),
                 ('w1', 'm2', 'bo@example.com');
             PRAGMA user_version = 3;",
        )
        .execute(&mut old)
        .await
        .unwrap();
        drop(old);

        let db = Db::open(&path).await.unwrap();
        let [(ada, ada_email), (bo, bo_email)] = accounts(&db).await.try_into().unwrap();
        let of: Vec<_> = member_accounts(&db)
            .await
            .into_iter()
            .map(|(_, _, account)| account.unwrap())
            .collect();

        assert_eq!(ada_email, "ada@example.com");
        assert_eq!(bo_email, "bo@example.com");
        assert_ne!(ada, bo);
        AccountId::try_from(ada.clone()).unwrap();
        assert_eq!(of, [ada.clone(), ada, bo]);
    }
}
