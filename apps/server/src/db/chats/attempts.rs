//! Each send of a request is an attempt, numbered in turn. What its core answers counts for
//! the latest attempt queued for its machine, and for any begun after it, which may have gone
//! out before it is known to.

use weacon_observability::Fault;
use weacon_protocol::{ChatEvent, ChatId, Receipt, RequestId};

use super::super::{Workspace, failed};
use super::Incoming;

/// Whether a request's core refused for now its latest attempt queued, or one after it, as SQL.
macro_rules! refused {
    () => {
        "(refused_attempt >= 0 AND refused_attempt >= delivered)"
    };
}
pub(super) use refused;

/// A request's send attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) number: i64,
}

/// Which request a receipt the server writes may end: one that `attempt` may answer, and when
/// `refused_only`, only while its core refused it for now.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Guard {
    pub(crate) attempt: i64,
    pub(crate) refused_only: bool,
}

impl Workspace {
    /// Begins a new send attempt of `request` at `at`, unless no open request is there to send.
    pub(crate) async fn begin_attempt(
        &self,
        request: &RequestId,
        at: i64,
    ) -> Result<Option<Attempt>, Fault> {
        let begun = sqlx::query_scalar(
            "UPDATE chat_requests SET attempt = attempt + 1, attempt_at = ?
             WHERE workspace_id = ? AND request_id = ? AND open = 1
             RETURNING attempt",
        )
        .bind(at)
        .bind(self.id.as_str())
        .bind(request.as_str())
        .fetch_optional(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(begun.map(|number| Attempt { number }))
    }

    /// Marks `attempt` of `request` queued for its machine, and no longer on its way unless
    /// another began since.
    pub(crate) async fn queued(&self, request: &RequestId, attempt: Attempt) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_requests SET delivered = MAX(delivered, ?1),
                 attempt_at = CASE WHEN attempt = ?1 THEN 0 ELSE attempt_at END
             WHERE workspace_id = ?2 AND request_id = ?3",
        )
        .bind(attempt.number)
        .bind(self.id.as_str())
        .bind(request.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Gives up `attempt` of `request`, which never went out: no longer on its way unless
    /// another began since.
    pub(crate) async fn abandon(&self, request: &RequestId, attempt: Attempt) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_requests SET attempt_at = 0
             WHERE workspace_id = ? AND request_id = ? AND attempt = ?",
        )
        .bind(self.id.as_str())
        .bind(request.as_str())
        .bind(attempt.number)
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Keeps that `request`'s core refused its `attempt` for now; whether that refusal counts,
    /// as no later attempt was queued since.
    pub(crate) async fn mark_refused(
        &self,
        request: &RequestId,
        attempt: i64,
    ) -> Result<bool, Fault> {
        let counts = sqlx::query_scalar(
            "UPDATE chat_requests SET refused_attempt = MAX(refused_attempt, ?1)
             WHERE workspace_id = ?2 AND request_id = ?3 AND open = 1 AND attempt >= ?1
             RETURNING ?1 >= delivered",
        )
        .bind(attempt)
        .bind(self.id.as_str())
        .bind(request.as_str())
        .fetch_optional(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(counts == Some(true))
    }

    /// Takes `request` out of the ledger after its `attempt` failed to send, unless it was ever
    /// queued or another attempt of it began since.
    pub(crate) async fn forget_request(
        &self,
        request: &RequestId,
        attempt: i64,
    ) -> Result<(), Fault> {
        sqlx::query(
            "DELETE FROM chat_requests
             WHERE workspace_id = ? AND request_id = ? AND delivered < 0 AND attempt = ?",
        )
        .bind(self.id.as_str())
        .bind(request.as_str())
        .bind(attempt)
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Ends `receipt`'s request to `chat` with it, a final receipt the server writes for the
    /// chat's machine, while it is open and `guard` holds; whether it did.
    pub(crate) async fn end_open(
        &self,
        chat: &ChatId,
        receipt: Receipt,
        at: i64,
        guard: Guard,
    ) -> Result<bool, Fault> {
        let mut transaction = self.db.write().await?;
        let open: bool = sqlx::query_scalar(concat!(
            "SELECT EXISTS (SELECT 1 FROM chat_requests WHERE workspace_id = ?1
                     AND chat_id = ?2 AND request_id = ?3 AND open = 1
                     AND ?4 >= delivered AND ?4 <= attempt AND (",
            refused!(),
            " OR NOT ?5))",
        ))
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(receipt.request.as_str())
        .bind(guard.attempt)
        .bind(guard.refused_only)
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        if !open {
            return Ok(false);
        }
        let event = ChatEvent::Receipt { receipt };
        let incoming = Incoming {
            core_seq: None,
            event: &event,
            at,
        };
        self.append(transaction, chat, &[incoming]).await?;
        Ok(true)
    }

    /// Whether a request to `chat` other than `request` is open and refused for now.
    pub(crate) async fn refused_before(
        &self,
        chat: &ChatId,
        request: &RequestId,
    ) -> Result<bool, Fault> {
        sqlx::query_scalar(concat!(
            "SELECT EXISTS (SELECT 1 FROM chat_requests WHERE workspace_id = ? AND chat_id = ?
                     AND request_id != ? AND open = 1 AND ",
            refused!(),
            ")",
        ))
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(request.as_str())
        .fetch_one(&self.db.readers)
        .await
        .map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::Outcome;
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, begin, id};
    use crate::test_support::two_teams;

    #[tokio::test]
    async fn an_answer_counts_for_the_latest_attempt_queued_and_any_begun_since() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        begin(&one, "c-1", "m1", NOW).await.unwrap();
        let request = id::<RequestId>("r-c-1");
        let first = one.begin_attempt(&request, NOW).await.unwrap().unwrap();
        one.queued(&request, first).await.unwrap();
        let receipt = Receipt {
            request: request.clone(),
            outcome: Outcome::Interrupted,
        };
        let guard = |attempt| Guard {
            attempt,
            refused_only: false,
        };

        // Two retries begin and neither goes out.
        for _ in 0..2 {
            let retry = one.begin_attempt(&request, NOW).await.unwrap().unwrap();
            assert!(
                !one.end_open(&id("c-1"), receipt.clone(), NOW, guard(0))
                    .await
                    .unwrap()
            );
            one.abandon(&request, retry).await.unwrap();
        }
        let refused = guard(first.number);
        let refused = Guard {
            refused_only: true,
            ..refused
        };
        assert!(
            !one.end_open(&id("c-1"), receipt.clone(), NOW, refused)
                .await
                .unwrap()
        );

        let ended = one
            .end_open(&id("c-1"), receipt, NOW, guard(first.number))
            .await;

        assert!(ended.unwrap());
        assert!(one.open_requests(&id("c-1")).await.unwrap().is_empty());
    }
}
