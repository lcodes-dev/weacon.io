use subtle::ConstantTimeEq;
use weacon_observability::Fault;
use weacon_protocol::{MemberId, WorkspaceChange, WorkspaceUpdate};

use super::{Workspace, failed, stored, unreadable};
use crate::email::Email;

/// How long a code works, in seconds.
pub(crate) const CODE_LIFETIME: i64 = 600;
/// How soon a member can be sent another code, in seconds.
const RESEND_AFTER: i64 = 60;
/// The most wrong codes a member may try in [`ATTEMPT_WINDOW`] seconds, whatever codes they
/// are sent meanwhile; so a code can't be guessed by asking for new ones.
const MAX_ATTEMPTS: i64 = 5;
const ATTEMPT_WINDOW: i64 = 3600;
const MEMBER_JOINED: &str = "member_joined";

/// One try at finishing a sign-in. Times are seconds since the Unix epoch.
pub(crate) struct Attempt<'a> {
    pub(crate) email: &'a Email,
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

/// The member a pending code is for, and that code.
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

    /// Replaces `member`'s sign-in code with one hashed as `code_hash`, unless they were sent
    /// one less than [`RESEND_AFTER`] seconds ago; says whether it did.
    pub(crate) async fn save_code(
        &self,
        member: &MemberId,
        code_hash: &[u8],
        now: i64,
    ) -> Result<bool, Fault> {
        let saved = sqlx::query(
            "INSERT INTO sign_in_codes
                 (workspace_id, member_id, code_hash, expires_at, sent_at, attempts, window_start)
             VALUES (?1, ?2, ?3, ?4 + ?5, ?4, 0, ?4)
             ON CONFLICT (workspace_id, member_id) DO UPDATE SET
                 code_hash = excluded.code_hash,
                 expires_at = excluded.expires_at,
                 sent_at = excluded.sent_at,
                 attempts = iif(window_start <= ?4 - ?6, 0, attempts),
                 window_start = iif(window_start <= ?4 - ?6, ?4, window_start)
             WHERE sent_at <= ?4 - ?7",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .bind(code_hash)
        .bind(now)
        .bind(CODE_LIFETIME)
        .bind(ATTEMPT_WINDOW)
        .bind(RESEND_AFTER)
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(saved.rows_affected() == 1)
    }

    /// Trades a code for a session: `None` when there is no such member, no live code, too
    /// many tries, or the wrong code, which counts as a try. A right code is used up.
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
             FROM members AS m JOIN sign_in_codes AS c
                 ON c.workspace_id = m.workspace_id AND c.member_id = m.member_id
             WHERE m.workspace_id = ? AND m.email = ?",
            )
            .bind(MEMBER_JOINED)
            .bind(self.id.as_str())
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
            "DELETE FROM sign_in_codes WHERE workspace_id = ? AND member_id = ?"
        } else {
            "UPDATE sign_in_codes SET attempts = attempts + 1
             WHERE workspace_id = ? AND member_id = ?"
        });
        spend
            .bind(self.id.as_str())
            .bind(&pending.member)
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
    use crate::db::Db;
    use crate::test_support::{member_id, workspace_id};

    const NOW: i64 = 100_000;

    fn email(text: &str) -> Email {
        Email::parse(text).unwrap()
    }

    fn id<T: TryFrom<String>>(text: &str) -> T {
        T::try_from(text.to_owned()).ok().unwrap()
    }

    // Workspaces w1 and w2 each with ada@example.com, as m1 and m2.
    async fn two_teams(dir: &TempDir) -> (Workspace, Workspace) {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let one = db
            .create_workspace(&workspace_id("w1"), "One")
            .await
            .unwrap();
        let two = db
            .create_workspace(&workspace_id("w2"), "Two")
            .await
            .unwrap();
        one.add_member(&member_id("m1"), &email("ada@example.com"))
            .await
            .unwrap();
        two.add_member(&member_id("m2"), &email("ada@example.com"))
            .await
            .unwrap();
        (one, two)
    }

    // A try at `code` by ada at `now`, for a session whose token is `token`.
    async fn try_code(team: &Workspace, code: &[u8], token: &[u8], now: i64) -> Option<Finished> {
        let ada = email("ada@example.com");
        let attempt = Attempt {
            email: &ada,
            code_hash: code,
            token_hash: token,
            now,
            token_expires_at: now + 100,
        };
        team.finish_sign_in(&attempt).await.unwrap()
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
        assert!(one.save_code(&id("m1"), b"code", NOW).await.unwrap());
        let first = try_code(&one, b"code", b"token-1", NOW).await;
        let again = try_code(&one, b"code", b"token-2", NOW).await;
        let later = NOW + RESEND_AFTER;
        assert!(one.save_code(&id("m1"), b"code-2", later).await.unwrap());
        let second = try_code(&one, b"code-2", b"token-3", later).await;

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
    async fn a_new_code_waits_a_minute_and_keeps_the_count_of_wrong_ones() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        assert!(one.save_code(&id("m1"), b"code", NOW).await.unwrap());
        assert!(
            !one.save_code(&id("m1"), b"code-2", NOW + RESEND_AFTER - 1)
                .await
                .unwrap()
        );
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(try_code(&one, b"guess", b"t", NOW).await, None);
        }
        let resent = NOW + RESEND_AFTER;
        assert!(one.save_code(&id("m1"), b"code-3", resent).await.unwrap());
        let still_locked = try_code(&one, b"code-3", b"t", resent).await;
        let window_over = NOW + ATTEMPT_WINDOW;
        assert!(
            one.save_code(&id("m1"), b"code-4", window_over)
                .await
                .unwrap()
        );
        let unlocked = try_code(&one, b"code-4", b"t", window_over).await;

        assert_eq!(still_locked, None);
        assert!(unlocked.is_some());
    }

    #[tokio::test]
    async fn the_fifth_wrong_code_is_the_last() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.save_code(&id("m1"), b"code", NOW).await.unwrap();
        for _ in 0..MAX_ATTEMPTS - 1 {
            assert_eq!(try_code(&one, b"guess", b"t", NOW).await, None);
        }

        assert!(try_code(&one, b"code", b"t", NOW).await.is_some());
    }

    #[tokio::test]
    async fn a_code_works_for_ten_minutes() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.save_code(&id("m1"), b"code", NOW).await.unwrap();

        assert_eq!(
            try_code(&one, b"code", b"t", NOW + CODE_LIFETIME).await,
            None
        );
        assert!(
            try_code(&one, b"code", b"t", NOW + CODE_LIFETIME - 1)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn sign_ins_and_sessions_stay_in_their_workspace() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        one.save_code(&id("m1"), b"code", NOW).await.unwrap();

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
        assert_eq!(try_code(&two, b"code", b"token", NOW).await, None);
        try_code(&one, b"code", b"token", NOW).await.unwrap();
        assert_eq!(
            one.session_end(b"token", NOW).await.unwrap(),
            Some(NOW + 100)
        );
        assert_eq!(two.session_end(b"token", NOW).await.unwrap(), None);
        assert_eq!(one.session_end(b"other", NOW).await.unwrap(), None);
        assert_eq!(one.session_end(b"token", NOW + 100).await.unwrap(), None);
        assert_eq!(one.changes_since(0, 10).await.unwrap().len(), 1);
        assert!(two.changes_since(0, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn expired_sessions_end_when_another_starts() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        one.save_code(&id("m1"), b"code", NOW).await.unwrap();
        try_code(&one, b"code", b"old", NOW).await.unwrap();
        let later = NOW + 100;
        one.save_code(&id("m1"), b"code", later).await.unwrap();
        try_code(&one, b"code", b"new", later).await.unwrap();
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
