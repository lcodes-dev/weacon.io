use sqlx::SqliteConnection;
use subtle::ConstantTimeEq;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, ChallengeId};

use super::owned::{NewWorkspace, ensure_personal};
use super::{Db, failed, stored, unreadable};
use crate::email::Email;

/// How long a code works, in seconds.
pub(crate) const CODE_LIFETIME: i64 = 600;
/// How many wrong tries cancel a challenge's code.
const MAX_ATTEMPTS: i64 = 5;

/// One try at finishing a sign-in. Times are seconds since the Unix epoch.
pub(crate) struct Attempt<'a> {
    pub(crate) email: &'a Email,
    pub(crate) challenge: &'a ChallengeId,
    pub(crate) code_hash: &'a [u8],
    pub(crate) token_hash: &'a [u8],
    pub(crate) now: i64,
    pub(crate) token_expires_at: i64,
    /// The account's Personal workspace, made with the session unless it has had one.
    pub(crate) personal: &'a NewWorkspace,
}

impl Db {
    /// Makes the account `id` for `email`; an address that has one already is `db.exists`.
    pub(crate) async fn add_account(&self, id: &AccountId, email: &Email) -> Result<(), Fault> {
        sqlx::query("INSERT INTO accounts (account_id, email) VALUES (?, ?)")
            .bind(id.as_str())
            .bind(email.as_str())
            .execute(&self.writer)
            .await
            .map_err(failed)?;
        Ok(())
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

    /// Records `challenge`, for `account`, with the code hashed as `code_hash`; and ends the
    /// expired challenges so they don't pile up.
    pub(crate) async fn start_challenge(
        &self,
        account: &AccountId,
        challenge: &ChallengeId,
        code_hash: &[u8],
        now: i64,
    ) -> Result<(), Fault> {
        let mut transaction = self.writer.begin().await.map_err(failed)?;
        sqlx::query("DELETE FROM sign_in_challenges WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query(
            "INSERT INTO sign_in_challenges
                 (challenge_id, account_id, code_hash, expires_at, attempts)
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(challenge.as_str())
        .bind(account.as_str())
        .bind(code_hash)
        .bind(now.saturating_add(CODE_LIFETIME))
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }

    /// Trades a challenge's code for a session, and gives the account: `None` when there is
    /// no such live challenge for that email, it has had too many tries, or the code is wrong,
    /// which counts as a try. A right code is used up.
    pub(crate) async fn finish_sign_in(
        &self,
        attempt: &Attempt<'_>,
    ) -> Result<Option<AccountId>, Fault> {
        let mut transaction = self
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let Some((account, code_hash)) = sqlx::query_as::<_, (String, Vec<u8>)>(
            "SELECT a.account_id, c.code_hash
             FROM sign_in_challenges AS c JOIN accounts AS a ON a.account_id = c.account_id
             WHERE c.challenge_id = ? AND a.email = ? AND c.expires_at > ? AND c.attempts < ?",
        )
        .bind(attempt.challenge.as_str())
        .bind(attempt.email.as_str())
        .bind(attempt.now)
        .bind(MAX_ATTEMPTS)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        else {
            return Ok(None);
        };
        let right = bool::from(code_hash.ct_eq(attempt.code_hash));
        let spend = sqlx::query(if right {
            "DELETE FROM sign_in_challenges WHERE challenge_id = ?"
        } else {
            "UPDATE sign_in_challenges SET attempts = attempts + 1 WHERE challenge_id = ?"
        });
        spend
            .bind(attempt.challenge.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        if right {
            start_session(&mut transaction, &account, attempt).await?;
            ensure_personal(&mut transaction, &account, attempt.personal).await?;
        }
        transaction.commit().await.map_err(failed)?;
        right.then(|| stored(account)).transpose()
    }

    /// The account, and its address, whose live session's token is hashed as `token_hash`.
    pub(crate) async fn session(
        &self,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<(AccountId, Email)>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT a.account_id, a.email
             FROM sessions AS s JOIN accounts AS a ON a.account_id = s.account_id
             WHERE s.token_hash = ? AND s.expires_at > ?",
        )
        .bind(token_hash)
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

    /// Ends the session whose token is hashed as `token_hash`, if there is one.
    pub(crate) async fn end_session(&self, token_hash: &[u8]) -> Result<(), Fault> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }
}

/// Starts a session, and ends the expired ones so they don't pile up.
async fn start_session(
    transaction: &mut SqliteConnection,
    account: &str,
    attempt: &Attempt<'_>,
) -> Result<(), Fault> {
    sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(attempt.now)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    sqlx::query("INSERT INTO sessions (token_hash, account_id, expires_at) VALUES (?, ?, ?)")
        .bind(attempt.token_hash)
        .bind(account)
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
    use crate::test_support::{email, member_id, workspace_id};

    fn personal(name: &str) -> NewWorkspace {
        NewWorkspace {
            id: workspace_id(&format!("p-{name}")),
            name: "Personal".to_owned(),
            owner: member_id(&format!("o-{name}")),
        }
    }

    const NOW: i64 = 100_000;

    fn id<T: TryFrom<String>>(text: &str) -> T {
        T::try_from(text.to_owned()).ok().unwrap()
    }

    // A database where ada@example.com has account a1 and bo@example.com has a2.
    async fn people(dir: &TempDir) -> Db {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        for (account, address) in [("a1", "ada@example.com"), ("a2", "bo@example.com")] {
            db.add_account(&id(account), &email(address)).await.unwrap();
        }
        db
    }

    // Starts challenge `name` for `account`, with `code`.
    async fn challenge(db: &Db, account: &str, name: &str, code: &[u8], now: i64) {
        db.start_challenge(&id(account), &id(name), code, now)
            .await
            .unwrap();
    }

    // A try by `who` at challenge `name` with `code` at `now`, and the account it signs in to;
    // a session it starts has `name` as its token.
    async fn try_as(db: &Db, who: &str, name: &str, code: &[u8], now: i64) -> Option<String> {
        let who = email(who);
        let challenge = id(name);
        let attempt = Attempt {
            email: &who,
            challenge: &challenge,
            code_hash: code,
            token_hash: name.as_bytes(),
            now,
            token_expires_at: now + 100,
            personal: &personal(name),
        };
        let account = db.finish_sign_in(&attempt).await.unwrap();
        account.map(|account| account.as_str().to_owned())
    }

    async fn try_code(db: &Db, name: &str, code: &[u8], now: i64) -> Option<String> {
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
        challenge(&db, "a1", "c1", b"code", NOW).await;
        let first = try_code(&db, "c1", b"code", NOW).await;
        let again = try_code(&db, "c1", b"code", NOW).await;
        challenge(&db, "a1", "c2", b"code-2", NOW).await;
        let second = try_code(&db, "c2", b"code-2", NOW).await;

        assert_eq!(first.as_deref(), Some("a1"));
        assert_eq!(again, None);
        assert_eq!(second.as_deref(), Some("a1"));
    }

    #[tokio::test]
    async fn the_fifth_wrong_code_is_the_last_for_its_challenge_only() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        for name in ["c1", "c2", "c3"] {
            challenge(&db, "a1", name, b"code", NOW).await;
        }
        for _ in 0..MAX_ATTEMPTS - 1 {
            assert_eq!(try_code(&db, "c1", b"guess", NOW).await, None);
        }
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(try_code(&db, "c2", b"guess", NOW).await, None);
        }

        assert!(try_code(&db, "c1", b"code", NOW).await.is_some());
        assert_eq!(try_code(&db, "c2", b"code", NOW).await, None);
        assert!(try_code(&db, "c3", b"code", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_challenge_is_only_for_its_account() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "a1", "c1", b"code", NOW).await;

        assert_eq!(
            try_as(&db, "bo@example.com", "c1", b"code", NOW).await,
            None
        );
        assert_eq!(
            try_as(&db, "cy@example.com", "c1", b"code", NOW).await,
            None
        );
        assert_eq!(try_code(&db, "c9", b"code", NOW).await, None);
        assert!(try_code(&db, "c1", b"code", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_code_works_for_ten_minutes_and_ended_ones_are_cleared() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "a1", "c1", b"code", NOW).await;
        challenge(&db, "a1", "c2", b"code", NOW).await;
        let ended = NOW + CODE_LIFETIME;

        assert_eq!(try_code(&db, "c1", b"code", ended).await, None);
        assert!(try_code(&db, "c2", b"code", ended - 1).await.is_some());
        challenge(&db, "a1", "c3", b"code", ended - 1).await;
        assert_eq!(count(&db, "sign_in_challenges").await, 2);
        challenge(&db, "a2", "c4", b"code", ended).await;
        assert_eq!(count(&db, "sign_in_challenges").await, 2);
    }

    #[tokio::test]
    async fn a_session_is_its_accounts_until_it_ends_or_is_ended() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "a1", "c1", b"code", NOW).await;
        challenge(&db, "a2", "c2", b"code", NOW).await;
        try_code(&db, "c1", b"code", NOW).await.unwrap();
        try_as(&db, "bo@example.com", "c2", b"code", NOW)
            .await
            .unwrap();
        let ends = NOW + 100;
        let ada = Some((id("a1"), email("ada@example.com")));
        let bo = Some((id("a2"), email("bo@example.com")));

        assert_eq!(db.session(b"c1", NOW).await.unwrap(), ada);
        assert_eq!(db.session(b"c2", ends - 1).await.unwrap(), bo);
        assert_eq!(db.session(b"other", NOW).await.unwrap(), None);
        assert_eq!(db.session(b"c1", ends).await.unwrap(), None);
        db.end_session(b"c1").await.unwrap();
        db.end_session(b"never").await.unwrap();
        assert_eq!(db.session(b"c1", NOW).await.unwrap(), None);
        assert_eq!(db.session(b"c2", NOW).await.unwrap(), bo);
    }

    #[tokio::test]
    async fn expired_sessions_end_when_another_starts() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "a1", "c1", b"code", NOW).await;
        try_code(&db, "c1", b"code", NOW).await.unwrap();
        let later = NOW + 100;
        challenge(&db, "a1", "c2", b"code", later).await;
        try_code(&db, "c2", b"code", later).await.unwrap();

        assert_eq!(count(&db, "sessions").await, 1);
    }
}
