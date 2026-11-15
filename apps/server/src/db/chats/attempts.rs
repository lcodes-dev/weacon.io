//! Each send of a request is an attempt; what answers it, or fails it, counts only while it is
//! the request's current one.

use weacon_observability::Fault;
use weacon_protocol::{ChatEvent, ChatId, Receipt, RequestId};

use super::super::{Workspace, failed};
use super::Incoming;

/// A request's send attempt, and how often its core had refused the request when it began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) number: i64,
    refusals: i64,
}

/// Which request a receipt the server writes may end: only one whose current attempt is
/// `attempt`, and when `refused_only`, only while its core refused that attempt.
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
        let begun = sqlx::query_as::<_, (i64, i64)>(
            "UPDATE chat_requests SET attempt = attempt + 1, attempt_at = ?
             WHERE workspace_id = ? AND request_id = ? AND open = 1
             RETURNING attempt, refusals",
        )
        .bind(at)
        .bind(self.id.as_str())
        .bind(request.as_str())
        .fetch_optional(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(begun.map(|(number, refusals)| Attempt { number, refusals }))
    }

    /// Marks `request` queued for its machine by `attempt`, and, when `attempt` is still current,
    /// no longer in flight and, unless its core refused it since, no longer refused.
    pub(crate) async fn queued(&self, request: &RequestId, attempt: Attempt) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_requests SET sent = 1,
                 refused = CASE WHEN attempt = ?1 AND refusals = ?2 THEN 0 ELSE refused END,
                 attempt_at = CASE WHEN attempt = ?1 THEN 0 ELSE attempt_at END
             WHERE workspace_id = ?3 AND request_id = ?4",
        )
        .bind(attempt.number)
        .bind(attempt.refusals)
        .bind(self.id.as_str())
        .bind(request.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Gives up `attempt` of `request`, which never went out: unless another began since, the
    /// one before it is current again, so its answers still count.
    pub(crate) async fn abandon(&self, request: &RequestId, attempt: Attempt) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_requests SET attempt = attempt - 1, attempt_at = 0
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

    /// Marks `request` refused for now by its core, when `attempt` is still its current one;
    /// whether it was.
    pub(crate) async fn mark_refused(
        &self,
        request: &RequestId,
        attempt: i64,
    ) -> Result<bool, Fault> {
        let marked = sqlx::query(
            "UPDATE chat_requests SET refused = 1, refusals = refusals + 1
             WHERE workspace_id = ? AND request_id = ? AND open = 1 AND attempt = ?",
        )
        .bind(self.id.as_str())
        .bind(request.as_str())
        .bind(attempt)
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(marked.rows_affected() > 0)
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
             WHERE workspace_id = ? AND request_id = ? AND sent = 0 AND attempt = ?",
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
        let open: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_requests WHERE workspace_id = ? AND chat_id = ?
                 AND request_id = ? AND open = 1 AND attempt = ? AND (refused = 1 OR NOT ?))",
        )
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

    /// Whether a request to `chat` other than `request` is open and its current attempt was
    /// refused for now.
    pub(crate) async fn refused_before(
        &self,
        chat: &ChatId,
        request: &RequestId,
    ) -> Result<bool, Fault> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_requests WHERE workspace_id = ? AND chat_id = ?
                 AND request_id != ? AND open = 1 AND refused = 1)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(request.as_str())
        .fetch_one(&self.db.readers)
        .await
        .map_err(failed)
    }
}
