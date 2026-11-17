use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, ChallengeId};

use super::account::{NewWorkspace, ensure_personal};
use super::{Db, exists, failed, stored, unreadable};
use crate::email::Email;

/// How many wrong tries end a challenge.
const MAX_ATTEMPTS: i64 = 5;

/// A sign-in code to record. Times are seconds since the Unix epoch.
pub(crate) struct NewChallenge<'a> {
    pub(crate) email: &'a Email,
    pub(crate) id: &'a ChallengeId,
    pub(crate) code: &'a str,
    pub(crate) now: i64,
    pub(crate) expires_at: i64,
}

/// One try at finishing a sign-in. Times are seconds since the Unix epoch.
pub(crate) struct Attempt<'a> {
    pub(crate) email: &'a Email,
    pub(crate) challenge: &'a ChallengeId,
    pub(crate) code: &'a str,
    pub(crate) token: &'a str,
    pub(crate) now: i64,
    pub(crate) token_expires_at: i64,
    /// The account's Personal workspace, made with the session unless it has had one.
    pub(crate) personal: &'a NewWorkspace,
}

impl Db {
    /// Makes the account `id` for `email`; an address that has one already is `db.exists`.
    pub(crate) async fn add_account(&self, id: &AccountId, email: &Email) -> Result<(), Fault> {
        let mut connection = self.writer.acquire().await.map_err(failed)?;
        if insert_account(&mut connection, id.as_str(), email).await? {
            Ok(())
        } else {
            Err(exists())
        }
    }

    /// The account with `email`, if there is one.
    pub(crate) async fn account_with(&self, email: &Email) -> Result<Option<AccountId>, Fault> {
        sqlx::query_scalar::<_, String>("SELECT account_id FROM accounts WHERE email = ?")
            .bind(email.as_str())
            .fetch_optional(&self.readers)
            .await
            .map_err(failed)?
            .map(stored)
            .transpose()
    }

    /// Records `new`, and ends the expired challenges so they don't pile up.
    pub(crate) async fn start_challenge(&self, new: &NewChallenge<'_>) -> Result<(), Fault> {
        let mut transaction = self.writer.begin().await.map_err(failed)?;
        sqlx::query("DELETE FROM sign_in_challenges WHERE expires_at <= ?")
            .bind(new.now)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query(
            "INSERT INTO sign_in_challenges
                 (challenge_id, email, code, expires_at, attempts)
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(new.id.as_str())
        .bind(new.email.as_str())
        .bind(new.code)
        .bind(new.expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }

    /// Trades a challenge's code for a session, and gives the account. `None` when there is no
    /// such live challenge for that email, the code is wrong, which counts as a try, or there
    /// is no account to sign in to. A right code is used up; a challenge that has had too many
    /// tries takes no more.
    pub(crate) async fn finish_sign_in(
        &self,
        attempt: &Attempt<'_>,
    ) -> Result<Option<AccountId>, Fault> {
        let mut transaction = self.write().await?;
        let code = sqlx::query_scalar::<_, String>(
            "SELECT code FROM sign_in_challenges
             WHERE challenge_id = ? AND email = ? AND expires_at > ? AND attempts < ?",
        )
        .bind(attempt.challenge.as_str())
        .bind(attempt.email.as_str())
        .bind(attempt.now)
        .bind(MAX_ATTEMPTS)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let Some(code) = code else {
            return Ok(None);
        };
        let account = if code == attempt.code {
            end_challenge(&mut transaction, attempt.challenge.as_str()).await?;
            account_of(&mut transaction, attempt.email).await?
        } else {
            wrong_try(&mut transaction, attempt.challenge.as_str()).await?;
            None
        };
        if let Some(account) = &account {
            start_session(&mut transaction, account, attempt).await?;
            ensure_personal(&mut transaction, account, attempt.personal).await?;
        }
        transaction.commit().await.map_err(failed)?;
        Ok(account)
    }

    /// The account, and its address, whose live session's token is `token`.
    pub(crate) async fn session(
        &self,
        token: &str,
        now: i64,
    ) -> Result<Option<(AccountId, Email)>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT a.account_id, a.email
             FROM sessions AS s JOIN accounts AS a ON a.account_id = s.account_id
             WHERE s.token = ? AND s.expires_at > ?",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&self.readers)
        .await
        .map_err(failed)?
        .map(|(account, email)| {
            let email = Email::parse(&email).map_err(|_| unreadable())?;
            Ok((stored(account)?, email))
        })
        .transpose()
    }

    /// Ends the session whose token is `token`, if there is one.
    pub(crate) async fn end_session(&self, token: &str) -> Result<(), Fault> {
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(&self.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }
}

/// Makes the account `id` for `email` unless the address has one, and says whether it did.
pub(super) async fn insert_account(
    connection: &mut SqliteConnection,
    id: &str,
    email: &Email,
) -> Result<bool, Fault> {
    let made = sqlx::query(
        "INSERT INTO accounts (account_id, email) VALUES (?, ?)
         ON CONFLICT (email) DO NOTHING",
    )
    .bind(id)
    .bind(email.as_str())
    .execute(connection)
    .await
    .map_err(failed)?;
    Ok(made.rows_affected() == 1)
}

async fn account_of(
    connection: &mut SqliteConnection,
    email: &Email,
) -> Result<Option<AccountId>, Fault> {
    sqlx::query_scalar("SELECT account_id FROM accounts WHERE email = ?")
        .bind(email.as_str())
        .fetch_optional(connection)
        .await
        .map_err(failed)?
        .map(stored)
        .transpose()
}

async fn end_challenge(connection: &mut SqliteConnection, challenge: &str) -> Result<(), Fault> {
    sqlx::query("DELETE FROM sign_in_challenges WHERE challenge_id = ?")
        .bind(challenge)
        .execute(connection)
        .await
        .map_err(failed)?;
    Ok(())
}

async fn wrong_try(connection: &mut SqliteConnection, challenge: &str) -> Result<(), Fault> {
    sqlx::query("UPDATE sign_in_challenges SET attempts = attempts + 1 WHERE challenge_id = ?")
        .bind(challenge)
        .execute(connection)
        .await
        .map_err(failed)?;
    Ok(())
}

/// Starts a session, and ends the expired ones so they don't pile up.
async fn start_session(
    transaction: &mut SqliteConnection,
    account: &AccountId,
    attempt: &Attempt<'_>,
) -> Result<(), Fault> {
    sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(attempt.now)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    sqlx::query("INSERT INTO sessions (token, account_id, expires_at) VALUES (?, ?, ?)")
        .bind(attempt.token)
        .bind(account.as_str())
        .bind(attempt.token_expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, id};
    use crate::test_support::{email, member_id, workspace_id};

    use crate::sign_in::CODE_LIFETIME;

    fn personal(name: &str) -> NewWorkspace {
        NewWorkspace {
            id: workspace_id(&format!("p-{name}")),
            name: "Personal".to_owned(),
            owner: member_id(&format!("o-{name}")),
        }
    }

    // A database where ada@example.com has account a1 and bo@example.com has a2.
    async fn people(dir: &TempDir) -> Db {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        for (account, address) in [("a1", "ada@example.com"), ("a2", "bo@example.com")] {
            db.add_account(&id(account), &email(address)).await.unwrap();
        }
        db
    }

    // Starts challenge `name` for the address `who`, with `code`.
    // A code that works for a code's lifetime from `now`.
    async fn challenge(db: &Db, who: &str, name: &str, code: &str, now: i64) {
        let (email, id) = (email(who), id(name));
        let new = NewChallenge {
            email: &email,
            id: &id,
            code,
            now,
            expires_at: now + CODE_LIFETIME,
        };
        db.start_challenge(&new).await.unwrap();
    }

    // A try by `who` at challenge `name` with `code` at `now`, and the account it signs in to;
    // a session it starts has `name` as its token.
    async fn try_as(db: &Db, who: &str, name: &str, code: &str, now: i64) -> Option<String> {
        let (who, challenge) = (email(who), id(name));
        finish(
            db,
            Attempt {
                email: &who,
                challenge: &challenge,
                code,
                token: name,
                now,
                token_expires_at: now + 100,
                personal: &personal(name),
            },
        )
        .await
    }

    async fn finish(db: &Db, attempt: Attempt<'_>) -> Option<String> {
        let account = db.finish_sign_in(&attempt).await.unwrap();
        account.map(|account| account.as_str().to_owned())
    }

    async fn try_code(db: &Db, name: &str, code: &str, now: i64) -> Option<String> {
        try_as(db, "ada@example.com", name, code, now).await
    }

    async fn count(db: &Db, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(&db.readers)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_address_has_one_account() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;

        let again = db.add_account(&id("a3"), &email("ada@example.com")).await;
        let taken = db.add_account(&id("a1"), &email("cy@example.com")).await;

        assert_eq!(again.unwrap_err().code().as_str(), "db.exists");
        assert_eq!(taken.unwrap_err().code().as_str(), "db.exists");
        for (address, account) in [
            ("ada@example.com", Some(id("a1"))),
            ("bo@example.com", Some(id("a2"))),
            ("cy@example.com", None),
        ] {
            assert_eq!(db.account_with(&email(address)).await.unwrap(), account);
        }
    }

    #[tokio::test]
    async fn a_right_code_signs_in_once() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", "code", NOW).await;
        let first = try_code(&db, "c1", "code", NOW).await;
        let again = try_code(&db, "c1", "code", NOW).await;
        challenge(&db, "ada@example.com", "c2", "code-2", NOW).await;
        let second = try_code(&db, "c2", "code-2", NOW).await;

        assert_eq!(first.as_deref(), Some("a1"));
        assert_eq!(again, None);
        assert_eq!(second.as_deref(), Some("a1"));
    }

    #[tokio::test]
    async fn the_fifth_wrong_code_is_the_last_for_its_challenge_only() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        for name in ["c1", "c2", "c3"] {
            challenge(&db, "ada@example.com", name, "code", NOW).await;
        }
        for _ in 0..MAX_ATTEMPTS - 1 {
            assert_eq!(try_code(&db, "c1", "guess", NOW).await, None);
        }
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(try_code(&db, "c2", "guess", NOW).await, None);
        }

        assert!(try_code(&db, "c1", "code", NOW).await.is_some());
        assert_eq!(try_code(&db, "c2", "code", NOW).await, None);
        assert!(try_code(&db, "c3", "code", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_challenge_is_only_for_its_address() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", "code", NOW).await;

        assert_eq!(try_as(&db, "bo@example.com", "c1", "code", NOW).await, None);
        assert_eq!(try_as(&db, "cy@example.com", "c1", "code", NOW).await, None);
        assert_eq!(try_code(&db, "c9", "code", NOW).await, None);
        assert!(try_code(&db, "c1", "code", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_challenge_takes_only_its_own_code_and_signs_in_only_an_account() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", "k1", NOW).await;
        challenge(&db, "ada@example.com", "c2", "k2", NOW).await;
        challenge(&db, "cy@example.com", "c3", "k3", NOW).await;

        assert_eq!(try_code(&db, "c1", "k2", NOW).await, None);
        assert_eq!(try_code(&db, "c2", "k2", NOW).await.as_deref(), Some("a1"));
        assert_eq!(try_as(&db, "cy@example.com", "c3", "k3", NOW).await, None);
        assert_eq!(count(&db, "sessions").await, 1);
    }

    #[tokio::test]
    async fn a_code_works_for_ten_minutes_and_ended_ones_are_cleared() {
        assert_eq!(CODE_LIFETIME, 600);
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", "k1", NOW).await;
        challenge(&db, "ada@example.com", "c2", "k2", NOW).await;
        let ended = NOW + CODE_LIFETIME;

        assert_eq!(try_code(&db, "c1", "k1", ended).await, None);
        assert!(try_code(&db, "c2", "k2", ended - 1).await.is_some());
        challenge(&db, "ada@example.com", "c3", "code", ended - 1).await;
        assert_eq!(count(&db, "sign_in_challenges").await, 2);
        challenge(&db, "bo@example.com", "c4", "code", ended).await;
        assert_eq!(count(&db, "sign_in_challenges").await, 2);
    }

    #[tokio::test]
    async fn a_session_is_its_accounts_until_it_ends_or_is_ended() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", "code", NOW).await;
        challenge(&db, "bo@example.com", "c2", "code", NOW).await;
        try_code(&db, "c1", "code", NOW).await.unwrap();
        try_as(&db, "bo@example.com", "c2", "code", NOW)
            .await
            .unwrap();
        let ends = NOW + 100;
        let ada = Some((id("a1"), email("ada@example.com")));
        let bo = Some((id("a2"), email("bo@example.com")));

        assert_eq!(db.session("c1", NOW).await.unwrap(), ada);
        assert_eq!(db.session("c2", ends - 1).await.unwrap(), bo);
        assert_eq!(db.session("other", NOW).await.unwrap(), None);
        assert_eq!(db.session("c1", ends).await.unwrap(), None);
        db.end_session("c1").await.unwrap();
        db.end_session("never").await.unwrap();
        assert_eq!(db.session("c1", NOW).await.unwrap(), None);
        assert_eq!(db.session("c2", NOW).await.unwrap(), bo);
    }

    #[tokio::test]
    async fn expired_sessions_end_when_another_starts() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", "code", NOW).await;
        try_code(&db, "c1", "code", NOW).await.unwrap();
        let later = NOW + 100;
        challenge(&db, "ada@example.com", "c2", "code", later).await;
        try_code(&db, "c2", "code", later).await.unwrap();

        assert_eq!(count(&db, "sessions").await, 1);
    }
}
