use sqlx::SqliteConnection;
use subtle::ConstantTimeEq;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, ChallengeId};

use super::owned::{NewWorkspace, ensure_personal};
use super::{Db, exists, failed, stored, unreadable};
use crate::email::Email;

/// How long a code works, in seconds.
pub(crate) const CODE_LIFETIME: i64 = 600;
/// How many wrong tries end a challenge.
const MAX_ATTEMPTS: i64 = 5;
/// How many live codes are mailed to one address, so a server open to sign-up can't flood an
/// inbox; `ada+1@` and `ada+2@` count apart, though they may reach the same one.
const MAX_LIVE: i64 = 3;

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
    /// The id of the account a right code makes, for an address that has none; `None` when
    /// the address may not sign up.
    pub(crate) new_account: Option<&'a AccountId>,
}

impl Db {
    /// Makes the account `id` for `email`, the server's owner if it is the first; an address
    /// that has one already is `db.exists`.
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

    /// Records `challenge` for `email`, with its code hashed as `code_hash`, and says whether
    /// the code may be mailed: not while the address has [`MAX_LIVE`] mailed live codes.
    pub(crate) async fn start_challenge(
        &self,
        email: &Email,
        challenge: &ChallengeId,
        code_hash: &[u8],
        now: i64,
    ) -> Result<bool, Fault> {
        let mut transaction = self.writer.begin().await.map_err(failed)?;
        sqlx::query("DELETE FROM sign_in_challenges WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        let mailed = sqlx::query_scalar(
            "INSERT INTO sign_in_challenges
                 (challenge_id, email, code_hash, expires_at, attempts, mailed)
             VALUES (?1, ?2, ?3, ?4, 0,
                 (SELECT count(*) FROM sign_in_challenges WHERE email = ?2 AND mailed) < ?5)
             RETURNING mailed",
        )
        .bind(challenge.as_str())
        .bind(email.as_str())
        .bind(code_hash)
        .bind(now.saturating_add(CODE_LIFETIME))
        .bind(MAX_LIVE)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)?;
        Ok(mailed)
    }

    /// Trades a code for a session, and gives the account, made now if the address had none
    /// and may sign up. The code may be any live one mailed to the address, so codes others
    /// asked for can't lock its owner out. `None` when there is no such live challenge for that
    /// email, the code is wrong, which counts as a try, or there is no account to sign in to.
    /// A right code is used up, and a challenge that has had too many tries ends.
    pub(crate) async fn finish_sign_in(
        &self,
        attempt: &Attempt<'_>,
    ) -> Result<Option<AccountId>, Fault> {
        let mut transaction = self
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let live = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM sign_in_challenges
             WHERE challenge_id = ? AND email = ? AND expires_at > ?",
        )
        .bind(attempt.challenge.as_str())
        .bind(attempt.email.as_str())
        .bind(attempt.now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        if live.is_none() {
            return Ok(None);
        }
        let account = if let Some(used) = matched_code(&mut transaction, attempt).await? {
            end_challenge(&mut transaction, &used).await?;
            account_for(&mut transaction, attempt).await?
        } else {
            wrong_try(&mut transaction, attempt.challenge.as_str()).await?;
            None
        };
        if let Some(account) = &account {
            start_session(&mut transaction, account, attempt).await?;
            ensure_personal(&mut transaction, account, attempt.personal).await?;
        }
        transaction.commit().await.map_err(failed)?;
        account.map(stored).transpose()
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

/// Makes the account `id` for `email` unless the address has one, and says whether it did;
/// the first account on the server is its owner.
pub(super) async fn insert_account(
    connection: &mut SqliteConnection,
    id: &str,
    email: &Email,
) -> Result<bool, Fault> {
    let made = sqlx::query(
        "INSERT INTO accounts (account_id, email, owner)
         VALUES (?, ?, NOT EXISTS (SELECT 1 FROM accounts))
         ON CONFLICT (email) DO NOTHING",
    )
    .bind(id)
    .bind(email.as_str())
    .execute(connection)
    .await
    .map_err(failed)?;
    Ok(made.rows_affected() == 1)
}

/// The account a right code signs in to: the address's own, or a new one if it may sign up.
async fn account_for(
    connection: &mut SqliteConnection,
    attempt: &Attempt<'_>,
) -> Result<Option<String>, Fault> {
    if let Some(new) = attempt.new_account {
        insert_account(&mut *connection, new.as_str(), attempt.email).await?;
    }
    sqlx::query_scalar("SELECT account_id FROM accounts WHERE email = ?")
        .bind(attempt.email.as_str())
        .fetch_optional(connection)
        .await
        .map_err(failed)
}

/// The challenge whose code `attempt` gives, among the live ones mailed to its address. Every
/// code is compared in full, so the time taken gives nothing away.
async fn matched_code(
    connection: &mut SqliteConnection,
    attempt: &Attempt<'_>,
) -> Result<Option<String>, Fault> {
    let codes = sqlx::query_as::<_, (String, Vec<u8>)>(
        "SELECT challenge_id, code_hash FROM sign_in_challenges
         WHERE email = ? AND expires_at > ? AND mailed",
    )
    .bind(attempt.email.as_str())
    .bind(attempt.now)
    .fetch_all(connection)
    .await
    .map_err(failed)?;
    Ok(codes
        .into_iter()
        .fold(None, |found, (challenge, code_hash)| {
            let right = bool::from(code_hash.ct_eq(attempt.code_hash));
            found.or_else(|| right.then_some(challenge))
        }))
}

async fn end_challenge(connection: &mut SqliteConnection, challenge: &str) -> Result<(), Fault> {
    sqlx::query("DELETE FROM sign_in_challenges WHERE challenge_id = ?")
        .bind(challenge)
        .execute(connection)
        .await
        .map_err(failed)?;
    Ok(())
}

/// Counts a wrong code against `challenge`, and ends it at [`MAX_ATTEMPTS`].
async fn wrong_try(connection: &mut SqliteConnection, challenge: &str) -> Result<(), Fault> {
    sqlx::query("UPDATE sign_in_challenges SET attempts = attempts + 1 WHERE challenge_id = ?")
        .bind(challenge)
        .execute(&mut *connection)
        .await
        .map_err(failed)?;
    sqlx::query("DELETE FROM sign_in_challenges WHERE challenge_id = ? AND attempts >= ?")
        .bind(challenge)
        .bind(MAX_ATTEMPTS)
        .execute(connection)
        .await
        .map_err(failed)?;
    Ok(())
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
    use crate::test_support::{member_id, workspace_id};

    fn personal(name: &str) -> NewWorkspace {
        NewWorkspace {
            id: workspace_id(&format!("p-{name}")),
            name: "Personal".to_owned(),
            owner: member_id(&format!("o-{name}")),
        }
    }

    const NOW: i64 = 100_000;

    fn email(text: &str) -> Email {
        Email::parse(text).unwrap()
    }

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

    // Starts challenge `name` for the address `who`, with `code`.
    async fn challenge(db: &Db, who: &str, name: &str, code: &[u8], now: i64) -> bool {
        db.start_challenge(&email(who), &id(name), code, now)
            .await
            .unwrap()
    }

    // A try by `who` at challenge `name` with `code` at `now`, and the account it signs in to;
    // a session it starts has `name` as its token.
    async fn try_as(db: &Db, who: &str, name: &str, code: &[u8], now: i64) -> Option<String> {
        let (who, challenge) = (email(who), id(name));
        finish(
            db,
            Attempt {
                email: &who,
                challenge: &challenge,
                code_hash: code,
                token_hash: name.as_bytes(),
                now,
                token_expires_at: now + 100,
                personal: &personal(name),
                new_account: None,
            },
        )
        .await
    }

    // The same, with the right code at NOW, where an address with no account gets `new`.
    async fn sign_up(db: &Db, who: &str, name: &str, new: &str) -> Option<String> {
        let (who, challenge, new) = (email(who), id(name), id(new));
        finish(
            db,
            Attempt {
                email: &who,
                challenge: &challenge,
                code_hash: b"code",
                token_hash: name.as_bytes(),
                now: NOW,
                token_expires_at: NOW + 100,
                personal: &personal(name),
                new_account: Some(&new),
            },
        )
        .await
    }

    async fn finish(db: &Db, attempt: Attempt<'_>) -> Option<String> {
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
        challenge(&db, "ada@example.com", "c1", b"code", NOW).await;
        let first = try_code(&db, "c1", b"code", NOW).await;
        let again = try_code(&db, "c1", b"code", NOW).await;
        challenge(&db, "ada@example.com", "c2", b"code-2", NOW).await;
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
            challenge(&db, "ada@example.com", name, b"code", NOW).await;
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
    async fn a_challenge_is_only_for_its_address() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", b"code", NOW).await;

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
        challenge(&db, "ada@example.com", "c1", b"code", NOW).await;
        challenge(&db, "ada@example.com", "c2", b"code", NOW).await;
        let ended = NOW + CODE_LIFETIME;

        assert_eq!(try_code(&db, "c1", b"code", ended).await, None);
        assert!(try_code(&db, "c2", b"code", ended - 1).await.is_some());
        challenge(&db, "ada@example.com", "c3", b"code", ended - 1).await;
        assert_eq!(count(&db, "sign_in_challenges").await, 2);
        challenge(&db, "bo@example.com", "c4", b"code", ended).await;
        assert_eq!(count(&db, "sign_in_challenges").await, 2);
    }

    #[tokio::test]
    async fn a_session_is_its_accounts_until_it_ends_or_is_ended() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        challenge(&db, "ada@example.com", "c1", b"code", NOW).await;
        challenge(&db, "bo@example.com", "c2", b"code", NOW).await;
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
        challenge(&db, "ada@example.com", "c1", b"code", NOW).await;
        try_code(&db, "c1", b"code", NOW).await.unwrap();
        let later = NOW + 100;
        challenge(&db, "ada@example.com", "c2", b"code", later).await;
        try_code(&db, "c2", b"code", later).await.unwrap();

        assert_eq!(count(&db, "sessions").await, 1);
    }

    async fn owners(db: &Db) -> Vec<String> {
        sqlx::query_scalar("SELECT account_id FROM accounts WHERE owner = 1")
            .fetch_all(&db.readers)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_first_account_is_the_servers_owner() {
        let dir = TempDir::new().unwrap();
        let made = people(&dir).await;
        let other = TempDir::new().unwrap();
        let signed_up = Db::create(&other.join("weacon.db")).await.unwrap();
        challenge(&signed_up, "cy@example.com", "c1", b"code", NOW).await;
        sign_up(&signed_up, "cy@example.com", "c1", "a3")
            .await
            .unwrap();
        signed_up
            .add_account(&id("a4"), &email("di@example.com"))
            .await
            .unwrap();
        let third = TempDir::new().unwrap();
        let added = Db::create(&third.join("weacon.db")).await.unwrap();
        let team = added.create_workspace(&id("w1"), "Team").await.unwrap();
        let (member, address) = (id("m1"), email("ed@example.com"));
        team.add_member(&member, &id("a5"), &address).await.unwrap();
        added
            .add_account(&id("a6"), &email("fay@example.com"))
            .await
            .unwrap();

        assert_eq!(owners(&made).await, ["a1"]);
        assert_eq!(owners(&signed_up).await, ["a3"]);
        assert_eq!(owners(&added).await, ["a5"]);
    }

    #[tokio::test]
    async fn a_right_code_makes_an_account_only_where_sign_up_is_allowed() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        for name in ["c1", "c2", "c3"] {
            challenge(&db, "cy@example.com", name, b"code", NOW).await;
        }
        challenge(&db, "ada@example.com", "c5", b"code", NOW).await;

        assert_eq!(
            try_as(&db, "cy@example.com", "c1", b"code", NOW).await,
            None
        );
        assert_eq!(
            try_as(&db, "cy@example.com", "c2", b"guess", NOW).await,
            None
        );
        assert_eq!(
            db.account_with(&email("cy@example.com")).await.unwrap(),
            None
        );
        // Using c1 freed a place.
        challenge(&db, "cy@example.com", "c4", b"code", NOW).await;
        assert_eq!(
            sign_up(&db, "cy@example.com", "c3", "a3").await.as_deref(),
            Some("a3")
        );
        // Once made, the account is the address's own.
        assert_eq!(
            sign_up(&db, "cy@example.com", "c4", "a9").await.as_deref(),
            Some("a3")
        );
        assert_eq!(
            sign_up(&db, "ada@example.com", "c5", "a9").await.as_deref(),
            Some("a1")
        );
        assert_eq!(
            db.account_with(&email("cy@example.com")).await.unwrap(),
            Some(id("a3"))
        );
        assert_eq!(owners(&db).await, ["a1"]);
        assert_eq!(
            db.session(b"c3", NOW).await.unwrap(),
            Some((id("a3"), email("cy@example.com")))
        );
    }

    #[tokio::test]
    async fn only_so_many_codes_are_mailed_to_an_address_at_once() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        let mut mailed = Vec::new();
        for (name, code) in [("c1", b"k1"), ("c2", b"k2"), ("c3", b"k3"), ("c4", b"k4")] {
            mailed.push(challenge(&db, "ada@example.com", name, code, NOW).await);
        }
        let other = challenge(&db, "bo@example.com", "c5", b"k5", NOW).await;
        // A code that was never mailed works nowhere, not even for its own challenge.
        let unmailed = try_code(&db, "c4", b"k4", NOW).await;
        let later = challenge(&db, "ada@example.com", "c6", b"k6", NOW + CODE_LIFETIME).await;

        assert_eq!(mailed, [true, true, true, false]);
        assert!(other);
        assert_eq!(unmailed, None);
        assert!(later);
    }

    #[tokio::test]
    async fn codes_others_asked_for_still_let_the_address_sign_in() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        // Someone else asks three times for ada, and those codes reach her inbox.
        for (name, code) in [("x1", b"k1"), ("x2", b"k2"), ("x3", b"k3")] {
            assert!(challenge(&db, "ada@example.com", name, code, NOW).await);
        }
        let own = challenge(&db, "ada@example.com", "c1", b"never-sent", NOW).await;
        let signed_in = try_code(&db, "c1", b"k2", NOW).await;
        let used = try_code(&db, "c1", b"k2", NOW).await;
        // Another address's codes don't count.
        let bo = try_as(&db, "bo@example.com", "x1", b"k1", NOW).await;

        assert!(!own);
        assert_eq!(signed_in.as_deref(), Some("a1"));
        assert_eq!(used, None);
        assert_eq!(bo, None);
        // Using k2 freed a place, so this one is mailed.
        assert!(challenge(&db, "ada@example.com", "c2", b"k4", NOW).await);
        assert_eq!(try_code(&db, "c2", b"k3", NOW).await.as_deref(), Some("a1"));
    }

    #[tokio::test]
    async fn a_challenge_that_has_had_too_many_tries_frees_its_place() {
        let dir = TempDir::new().unwrap();
        let db = people(&dir).await;
        for name in ["x1", "x2", "x3"] {
            challenge(&db, "ada@example.com", name, b"code", NOW).await;
        }
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(try_code(&db, "x1", b"guess", NOW).await, None);
        }

        assert_eq!(count(&db, "sign_in_challenges").await, 2);
        assert!(challenge(&db, "ada@example.com", "c1", b"mine", NOW).await);
        assert_eq!(
            try_code(&db, "c1", b"mine", NOW).await.as_deref(),
            Some("a1")
        );
    }
}
