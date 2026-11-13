use subtle::ConstantTimeEq;
use weacon_observability::Fault;
use weacon_protocol::{ChallengeId, MemberId, WorkspaceChange, WorkspaceUpdate};

use super::{Workspace, failed, stored, unreadable};
use crate::email::Email;

/// How long a code works, in seconds.
pub(crate) const CODE_LIFETIME: i64 = 600;
/// How many wrong tries cancel a challenge's code.
const MAX_ATTEMPTS: i64 = 5;
const MEMBER_JOINED: &str = "member_joined";

/// One try at finishing a sign-in. Times are seconds since the Unix epoch.
pub(crate) struct Attempt<'a> {
    pub(crate) email: &'a Email,
    pub(crate) challenge: &'a ChallengeId,
    pub(crate) code_hash: &'a [u8],
    pub(crate) token_hash: &'a [u8],
    pub(crate) now: i64,
    pub(crate) token_expires_at: i64,
}

/// A finished sign-in; `joined` when it was the member's first, which is a change.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Finished {
    pub(crate) member: MemberId,
    pub(crate) joined: bool,
}

/// The member a challenge is for, and its code.
struct Pending {
    member: String,
    code_hash: Vec<u8>,
    expires_at: i64,
    attempts: i64,
    joined: bool,
}

impl Workspace {
    /// The member with `email`, if there is one.
    pub(crate) async fn member_with(&self, email: &Email) -> Result<Option<MemberId>, Fault> {
        sqlx::query_scalar::<_, String>(
            "SELECT member_id FROM members WHERE workspace_id = ? AND email = ?",
        )
        .bind(self.id.as_str())
        .bind(email.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(stored)
        .transpose()
    }

    /// Records `challenge`, for `member`, with the code hashed as `code_hash`; and ends this
    /// workspace's expired challenges so they don't pile up.
    pub(crate) async fn start_challenge(
        &self,
        member: &MemberId,
        challenge: &ChallengeId,
        code_hash: &[u8],
        now: i64,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        sqlx::query("DELETE FROM sign_in_challenges WHERE workspace_id = ? AND expires_at <= ?")
            .bind(self.id.as_str())
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query(
            "INSERT INTO sign_in_challenges
                 (workspace_id, challenge_id, member_id, code_hash, expires_at, attempts)
             VALUES (?, ?, ?, ?, ?, 0)",
        )
        .bind(self.id.as_str())
        .bind(challenge.as_str())
        .bind(member.as_str())
        .bind(code_hash)
        .bind(now.saturating_add(CODE_LIFETIME))
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }

    /// Trades a challenge's code for a session: `None` when there is no such challenge for
    /// that email, it has ended, it has had too many tries, or the code is wrong, which counts
    /// as a try. A right code is used up.
    pub(crate) async fn finish_sign_in(
        &self,
        attempt: &Attempt<'_>,
    ) -> Result<Option<Finished>, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let Some((member, code_hash, expires_at, attempts, joined)) =
            sqlx::query_as::<_, (String, Vec<u8>, i64, i64, bool)>(
                "SELECT m.member_id, c.code_hash, c.expires_at, c.attempts,
                 EXISTS (SELECT 1 FROM changes AS j
                     WHERE j.workspace_id = m.workspace_id AND j.member_id = m.member_id
                         AND j.kind = ?) AS joined
             FROM sign_in_challenges AS c JOIN members AS m
                 ON m.workspace_id = c.workspace_id AND m.member_id = c.member_id
             WHERE c.workspace_id = ? AND c.challenge_id = ? AND m.email = ?",
            )
            .bind(MEMBER_JOINED)
            .bind(self.id.as_str())
            .bind(attempt.challenge.as_str())
            .bind(attempt.email.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(failed)?
        else {
            return Ok(None);
        };
        let pending = Pending {
            member,
            code_hash,
            expires_at,
            attempts,
            joined,
        };
        if !self.spend_code(&mut transaction, &pending, attempt).await? {
            transaction.commit().await.map_err(failed)?;
            return Ok(None);
        }
        self.start_session(&mut transaction, &pending.member, attempt)
            .await?;
        if !pending.joined {
            self.join(&mut transaction, &pending.member).await?;
        }
        transaction.commit().await.map_err(failed)?;
        Ok(Some(Finished {
            member: stored(pending.member)?,
            joined: !pending.joined,
        }))
    }

    /// Whether `pending`'s code is live and right; a right one is used up, and a wrong one
    /// counts as a try.
    async fn spend_code(
        &self,
        transaction: &mut sqlx::SqliteConnection,
        pending: &Pending,
        attempt: &Attempt<'_>,
    ) -> Result<bool, Fault> {
        if pending.expires_at <= attempt.now || pending.attempts >= MAX_ATTEMPTS {
            return Ok(false);
        }
        let right = bool::from(pending.code_hash.ct_eq(attempt.code_hash));
        let spend = sqlx::query(if right {
            "DELETE FROM sign_in_challenges WHERE workspace_id = ? AND challenge_id = ?"
        } else {
            "UPDATE sign_in_challenges SET attempts = attempts + 1
             WHERE workspace_id = ? AND challenge_id = ?"
        });
        spend
            .bind(self.id.as_str())
            .bind(attempt.challenge.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        Ok(right)
    }

    /// Starts a session, and ends this workspace's expired ones so they don't pile up.
    async fn start_session(
        &self,
        transaction: &mut sqlx::SqliteConnection,
        member: &str,
        attempt: &Attempt<'_>,
    ) -> Result<(), Fault> {
        sqlx::query("DELETE FROM sessions WHERE workspace_id = ? AND expires_at <= ?")
            .bind(self.id.as_str())
            .bind(attempt.now)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query(
            "INSERT INTO sessions (workspace_id, token_hash, member_id, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(attempt.token_hash)
        .bind(member)
        .bind(attempt.token_expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Records that `member` joined, as the workspace's next change.
    async fn join(
        &self,
        transaction: &mut sqlx::SqliteConnection,
        member: &str,
    ) -> Result<(), Fault> {
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id)
             SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3 FROM changes WHERE workspace_id = ?1",
        )
        .bind(self.id.as_str())
        .bind(MEMBER_JOINED)
        .bind(member)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// When the live session whose token is hashed as `token_hash` ends, in seconds since the
    /// Unix epoch.
    pub(crate) async fn session_end(
        &self,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<i64>, Fault> {
        sqlx::query_scalar(
            "SELECT expires_at FROM sessions
             WHERE workspace_id = ? AND token_hash = ? AND expires_at > ?",
        )
        .bind(self.id.as_str())
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)
    }

    /// Up to `limit` changes after `seq`, in order.
    pub(crate) async fn changes_since(
        &self,
        seq: u64,
        limit: u32,
    ) -> Result<Vec<WorkspaceUpdate>, Fault> {
        let after = i64::try_from(seq).unwrap_or(i64::MAX);
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT seq, kind, member_id FROM changes
             WHERE workspace_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(after)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(seq, kind, member)| self.update(seq, &kind, member))
        .collect()
    }

    fn update(&self, seq: i64, kind: &str, member: String) -> Result<WorkspaceUpdate, Fault> {
        let change = match kind {
            MEMBER_JOINED => WorkspaceChange::MemberJoined {
                member: stored(member)?,
            },
            // This server writes every kind it stores, so another is damage, not news.
            _ => return Err(unreadable()),
        };
        Ok(WorkspaceUpdate {
            workspace: self.id.clone(),
            seq: u64::try_from(seq).map_err(|_| unreadable())?,
            change,
        })
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    use crate::test_support::{fresh_account, two_teams};

    const NOW: i64 = 100_000;

    fn email(text: &str) -> Email {
        Email::parse(text).unwrap()
    }

    fn id<T: TryFrom<String>>(text: &str) -> T {
        T::try_from(text.to_owned()).ok().unwrap()
    }

    // Starts challenge `name` for `member` in `team`, with `code`.
    async fn challenge(team: &Workspace, member: &str, name: &str, code: &[u8], now: i64) {
        team.start_challenge(&id(member), &id(name), code, now)
            .await
            .unwrap();
    }

    // A try by `who` at challenge `name` with `code` at `now`; a session it starts has `name` as its token.
    async fn try_as(
        team: &Workspace,
        who: &str,
        name: &str,
        code: &[u8],
        now: i64,
    ) -> Option<Finished> {
        let who = email(who);
        let challenge = id(name);
        let attempt = Attempt {
            email: &who,
            challenge: &challenge,
            code_hash: code,
            token_hash: name.as_bytes(),
            now,
            token_expires_at: now + 100,
        };
        team.finish_sign_in(&attempt).await.unwrap()
    }

    async fn try_code(team: &Workspace, name: &str, code: &[u8], now: i64) -> Option<Finished> {
        try_as(team, "ada@example.com", name, code, now).await
    }

    async fn challenges(team: &Workspace) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM sign_in_challenges")
            .fetch_one(&team.db.readers)
            .await
            .unwrap()
    }

    fn joined(seq: u64, member: &str) -> WorkspaceUpdate {
        WorkspaceUpdate {
            workspace: id("w1"),
            seq,
            change: WorkspaceChange::MemberJoined { member: id(member) },
        }
    }

    #[tokio::test]
    async fn a_right_code_signs_in_once_and_the_first_time_joins() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        challenge(&one, "m1", "c1", b"code", NOW).await;
        let first = try_code(&one, "c1", b"code", NOW).await;
        let again = try_code(&one, "c1", b"code", NOW).await;
        challenge(&one, "m1", "c2", b"code-2", NOW).await;
        let second = try_code(&one, "c2", b"code-2", NOW).await;

        assert_eq!(
            first,
            Some(Finished {
                member: id("m1"),
                joined: true,
            })
        );
        assert_eq!(again, None);
        assert_eq!(
            second,
            Some(Finished {
                member: id("m1"),
                joined: false,
            })
        );
        assert_eq!(one.changes_since(0, 10).await.unwrap(), [joined(1, "m1")]);
        assert!(one.changes_since(1, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_fifth_wrong_code_is_the_last_for_its_challenge_only() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        for name in ["c1", "c2", "c3"] {
            challenge(&one, "m1", name, b"code", NOW).await;
        }
        for _ in 0..MAX_ATTEMPTS - 1 {
            assert_eq!(try_code(&one, "c1", b"guess", NOW).await, None);
        }
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(try_code(&one, "c2", b"guess", NOW).await, None);
        }

        assert!(try_code(&one, "c1", b"code", NOW).await.is_some());
        assert_eq!(try_code(&one, "c2", b"code", NOW).await, None);
        assert!(try_code(&one, "c3", b"code", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_challenge_is_only_for_its_member() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.add_member(&id("m3"), &fresh_account(), &email("bo@example.com"))
            .await
            .unwrap();
        challenge(&one, "m1", "c1", b"code", NOW).await;

        assert_eq!(
            try_as(&one, "bo@example.com", "c1", b"code", NOW).await,
            None
        );
        assert_eq!(try_code(&one, "c9", b"code", NOW).await, None);
        assert!(try_code(&one, "c1", b"code", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_code_works_for_ten_minutes_and_ended_ones_are_cleared() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        challenge(&one, "m1", "c1", b"code", NOW).await;
        challenge(&one, "m1", "c2", b"code", NOW).await;
        let ended = NOW + CODE_LIFETIME;

        assert_eq!(try_code(&one, "c1", b"code", ended).await, None);
        assert!(try_code(&one, "c2", b"code", ended - 1).await.is_some());
        challenge(&one, "m1", "c3", b"code", ended - 1).await;
        assert_eq!(challenges(&one).await, 2);
        challenge(&one, "m1", "c4", b"code", ended).await;
        assert_eq!(challenges(&one).await, 2);
    }

    #[tokio::test]
    async fn sign_ins_and_sessions_stay_in_their_workspace() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        challenge(&one, "m1", "c1", b"code", NOW).await;
        challenge(&two, "m2", "c2", b"code", NOW).await;

        assert_eq!(
            one.member_with(&email("ada@example.com")).await.unwrap(),
            Some(id("m1"))
        );
        assert_eq!(
            two.member_with(&email("ada@example.com")).await.unwrap(),
            Some(id("m2"))
        );
        assert_eq!(
            one.member_with(&email("bo@example.com")).await.unwrap(),
            None
        );
        assert_eq!(try_code(&two, "c1", b"code", NOW).await, None);
        try_code(&one, "c1", b"code", NOW).await.unwrap();
        assert_eq!(one.session_end(b"c1", NOW).await.unwrap(), Some(NOW + 100));
        assert_eq!(two.session_end(b"c1", NOW).await.unwrap(), None);
        assert_eq!(one.session_end(b"other", NOW).await.unwrap(), None);
        assert_eq!(one.session_end(b"c1", NOW + 100).await.unwrap(), None);
        assert_eq!(one.changes_since(0, 10).await.unwrap().len(), 1);
        assert!(two.changes_since(0, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn expired_sessions_end_when_another_starts() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        challenge(&one, "m1", "c1", b"code", NOW).await;
        try_code(&one, "c1", b"code", NOW).await.unwrap();
        let later = NOW + 100;
        challenge(&one, "m1", "c2", b"code", later).await;
        try_code(&one, "c2", b"code", later).await.unwrap();
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
            .fetch_one(&one.db.readers)
            .await
            .unwrap();

        assert_eq!(sessions, 1);
    }

    #[tokio::test]
    async fn changes_come_in_pages_and_an_unknown_kind_is_unreadable() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        sqlx::query(
            "INSERT INTO changes (workspace_id, seq, kind, member_id)
             VALUES ('w1', 1, 'member_joined', 'm1'), ('w1', 2, 'member_joined', 'm1'),
                    ('w1', 3, 'x', 'm1')",
        )
        .execute(&one.db.writer)
        .await
        .unwrap();

        assert_eq!(
            one.changes_since(0, 2).await.unwrap(),
            [joined(1, "m1"), joined(2, "m1")]
        );
        assert_eq!(
            one.changes_since(2, 2).await.unwrap_err().code().as_str(),
            "db.unreadable"
        );
    }
}
