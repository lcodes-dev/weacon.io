use weacon_observability::Fault;
use weacon_protocol::{AccountId, MemberId, WorkspaceId, WorkspaceSummary};

use super::account::summary;
use super::sign_in::insert_account;
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
pub(crate) struct MemberRow {
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
        insert_account(&mut transaction, new_account.as_str(), email).await?;
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

    /// The member whose account has the live session with the token hashed as `token_hash`.
    pub(crate) async fn session_member(
        &self,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<MemberId>, Fault> {
        sqlx::query_scalar::<_, String>(
            "SELECT m.member_id FROM sessions AS s JOIN members AS m ON m.account_id = s.account_id
             WHERE m.workspace_id = ? AND s.token_hash = ? AND s.expires_at > ?",
        )
        .bind(self.id.as_str())
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(stored)
        .transpose()
    }

    /// Ends `member`'s membership for whoever has it, keeping what they did here.
    #[cfg(test)]
    pub(crate) async fn unlink(&self, member: &MemberId) {
        sqlx::query(
            "UPDATE members SET account_id = NULL WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .execute(&self.db.writer)
        .await
        .unwrap();
    }

    /// The members, in the order they were added.
    pub(crate) async fn members(&self) -> Result<Vec<MemberRow>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT member_id, email FROM members WHERE workspace_id = ? ORDER BY rowid",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, email)| {
            Ok(MemberRow {
                id: stored(id)?,
                email: Email::parse(&email).map_err(|_| unreadable())?,
            })
        })
        .collect()
    }

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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, raw_sql};
    use weacon_testing::TempDir;

    use weacon_protocol::ChallengeId;

    use super::*;
    use crate::db::schema::STEPS;
    use crate::db::{Attempt, NewWorkspace};
    use crate::test_support::{email, fresh_account, member_id, two_teams, workspace_id};

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

    // Signs `address` in at time 100 with a session that ends at 200, whose token is `token`.
    async fn session(db: &Db, address: &str, token: &[u8]) {
        let (address, challenge) = (email(address), ChallengeId::try_from("c".to_owned()));
        let challenge = challenge.unwrap();
        db.start_challenge(&address, &challenge, b"code", 100)
            .await
            .unwrap();
        let attempt = Attempt {
            email: &address,
            challenge: &challenge,
            code_hash: b"code",
            token_hash: token,
            now: 100,
            token_expires_at: 200,
            personal: &NewWorkspace {
                id: workspace_id(&format!("p-{}", token.len())),
                name: "Personal".to_owned(),
                owner: member_id("owner"),
            },
            new_account: None,
        };
        db.finish_sign_in(&attempt).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_session_is_a_member_only_where_its_account_is_and_while_it_lasts() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        session(&one.db, "ada@example.com", b"ada").await;
        session(&one.db, "bo@example.com", b"bo").await;
        let member = |id| Some(member_id(id));

        assert_eq!(one.session_member(b"ada", 100).await.unwrap(), member("m1"));
        assert_eq!(two.session_member(b"ada", 199).await.unwrap(), member("m2"));
        assert_eq!(one.session_member(b"bo", 100).await.unwrap(), member("m3"));
        assert_eq!(two.session_member(b"bo", 100).await.unwrap(), None);
        assert_eq!(one.session_member(b"other", 100).await.unwrap(), None);
        assert_eq!(one.session_member(b"ada", 200).await.unwrap(), None);
        one.unlink(&member_id("m1")).await;
        assert_eq!(one.session_member(b"ada", 100).await.unwrap(), None);
        assert_eq!(two.session_member(b"ada", 100).await.unwrap(), member("m2"));
    }

    async fn rows(db: &Db, table: &str) -> i64 {
        let count = format!("SELECT count(*) FROM {table}");
        sqlx::query_scalar(sqlx::AssertSqlSafe(count))
            .fetch_one(&db.readers)
            .await
            .unwrap()
    }

    async fn owners(db: &Db) -> Vec<String> {
        sqlx::query_scalar("SELECT account_id FROM accounts WHERE owner = 1")
            .fetch_all(&db.readers)
            .await
            .unwrap()
    }

    // A database at `path` from before accounts: ada in w1 and w2, bo in w1, and a session
    // and a sign-in of ada's in w1.
    async fn old_database(path: &std::path::Path) {
        let mut old = SqliteConnectOptions::new()
            .filename(path)
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
             INSERT INTO sessions VALUES ('w1', x'01', 'm1', 9999999999);
             INSERT INTO sign_in_challenges VALUES ('w1', 'c1', 'm1', x'02', 9999999999, 0);
             PRAGMA user_version = 3;",
        )
        .execute(&mut old)
        .await
        .unwrap();
        drop(old);
    }

    #[tokio::test]
    async fn members_from_before_accounts_get_one_per_address() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        old_database(&path).await;

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
        assert_eq!(of, [ada.clone(), ada.clone(), bo]);
        // Members from before may do what any member may.
        let ada_id = AccountId::try_from(ada.clone()).unwrap();
        let roles = db.account(ada_id).workspaces().await.unwrap();
        assert!(
            roles
                .iter()
                .all(|seen| seen.role == weacon_protocol::WorkspaceRole::Member)
        );
        assert_eq!(owners(&db).await, [ada.as_str()]);
        // Sign-ins from before were to one workspace, and don't carry over.
        assert_eq!(rows(&db, "sessions").await, 0);
        assert_eq!(rows(&db, "sign_in_challenges").await, 0);
        let twice = sqlx::query(
            "INSERT INTO members (workspace_id, member_id, email, account_id)
             VALUES ('w1', 'm9', 'x@example.com', ?)",
        )
        .bind(ada)
        .execute(&db.writer)
        .await;
        assert_eq!(failed(twice.unwrap_err()).code().as_str(), "db.exists");
    }

    async fn with_members(db: &Db, id: &str, members: &[(&str, &str)]) -> Workspace {
        let workspace = db
            .create_workspace(&workspace_id(id), "Team")
            .await
            .unwrap();
        for (member, address) in members {
            workspace
                .add_member(&member_id(member), &fresh_account(), &email(address))
                .await
                .unwrap();
        }
        workspace
    }

    #[tokio::test]
    async fn every_read_sees_only_its_own_workspace() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let one = with_members(&db, "w1", &[("m1", "ada@example.com")]).await;
        let two = with_members(
            &db,
            "w2",
            &[("m2", "ada@example.com"), ("m3", "bo@example.com")],
        )
        .await;
        let member = |id: &str, address: &str| MemberRow {
            id: member_id(id),
            email: email(address),
        };

        assert_eq!(
            db.workspace(&workspace_id("w1"))
                .await
                .unwrap()
                .members()
                .await
                .unwrap(),
            [member("m1", "ada@example.com")]
        );
        assert_eq!(
            db.workspace(&workspace_id("w3"))
                .await
                .unwrap_err()
                .code()
                .as_str(),
            "workspace.not_found"
        );
        assert_eq!(
            one.members().await.unwrap(),
            [member("m1", "ada@example.com")]
        );
        assert_eq!(
            two.members().await.unwrap(),
            [
                member("m2", "ada@example.com"),
                member("m3", "bo@example.com")
            ]
        );
    }

    #[tokio::test]
    async fn duplicates_are_refused_as_existing() {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let one = db
            .create_workspace(&workspace_id("w1"), "One")
            .await
            .unwrap();
        one.add_member(
            &member_id("m1"),
            &fresh_account(),
            &email("ada@example.com"),
        )
        .await
        .unwrap();

        let same_email = one
            .add_member(
                &member_id("m2"),
                &fresh_account(),
                &email("ada@example.com"),
            )
            .await
            .unwrap_err();
        let id = one
            .add_member(&member_id("m1"), &fresh_account(), &email("bo@example.com"))
            .await
            .unwrap_err();
        let workspace = db
            .create_workspace(&workspace_id("w1"), "Again")
            .await
            .unwrap_err();
        assert_eq!(same_email.code().as_str(), "db.exists");
        assert_eq!(id.code().as_str(), "db.exists");
        assert_eq!(workspace.code().as_str(), "db.exists");
    }
}
