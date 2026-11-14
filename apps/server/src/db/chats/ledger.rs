use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{ChatId, MemberId, RequestId};

use super::super::{Workspace, failed, stored};

/// A command a member sent a chat, as the ledger keeps it.
#[derive(Debug)]
pub(crate) struct NewRequest<'a> {
    pub(crate) id: &'a RequestId,
    pub(crate) chat: &'a ChatId,
    pub(crate) member: &'a MemberId,
    /// Tells a retry from another command under the same id.
    pub(crate) action_hash: &'a [u8],
    pub(crate) at: i64,
}

/// Whether a request was seen before.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recorded {
    New,
    /// The same command again; `open` until its receipt ended it.
    Same {
        open: bool,
    },
    /// Another command under the same id.
    Conflict,
}

impl Workspace {
    /// Records `request` in the ledger, unless it is there already.
    pub(crate) async fn record_request(&self, request: &NewRequest<'_>) -> Result<Recorded, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let earlier = sqlx::query_as::<_, (String, Vec<u8>, i64)>(
            "SELECT chat_id, action_hash, open FROM chat_requests
             WHERE workspace_id = ? AND request_id = ?",
        )
        .bind(self.id.as_str())
        .bind(request.id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let recorded = match earlier {
            None => {
                self.insert_request(&mut transaction, request).await?;
                Recorded::New
            }
            Some((chat, hash, open))
                if chat == request.chat.as_str() && hash == request.action_hash =>
            {
                Recorded::Same { open: open != 0 }
            }
            Some(_) => Recorded::Conflict,
        };
        transaction.commit().await.map_err(failed)?;
        Ok(recorded)
    }

    /// Closes `request`, whose receipt ended it.
    pub(crate) async fn end_request(&self, request: &RequestId) -> Result<(), Fault> {
        sqlx::query("UPDATE chat_requests SET open = 0 WHERE workspace_id = ? AND request_id = ?")
            .bind(self.id.as_str())
            .bind(request.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// The requests to `chat` no receipt has ended yet, oldest first, with when each was sent.
    pub(crate) async fn open_requests(
        &self,
        chat: &ChatId,
    ) -> Result<Vec<(RequestId, i64)>, Fault> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT request_id, sent_at FROM chat_requests
             WHERE workspace_id = ? AND chat_id = ? AND open = 1 ORDER BY sent_at, rowid",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, sent_at)| Ok((stored(id)?, sent_at)))
        .collect()
    }

    pub(super) async fn insert_request(
        &self,
        transaction: &mut SqliteConnection,
        request: &NewRequest<'_>,
    ) -> Result<(), Fault> {
        sqlx::query(
            "INSERT INTO chat_requests
                 (workspace_id, request_id, chat_id, member_id, action_hash, sent_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(request.id.as_str())
        .bind(request.chat.as_str())
        .bind(request.member.as_str())
        .bind(request.action_hash)
        .bind(request.at)
        .execute(transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::super::test_support::{NOW, id, make};
    use super::*;
    use crate::test_support::{member_id, two_teams};

    async fn record(
        workspace: &Workspace,
        request: &str,
        chat: &ChatId,
        hash: &[u8],
        at: i64,
    ) -> Recorded {
        let (request, member) = (id::<RequestId>(request), member_id("m1"));
        let new = NewRequest {
            id: &request,
            chat,
            member: &member,
            action_hash: hash,
            at,
        };
        workspace.record_request(&new).await.unwrap()
    }

    fn opened(list: Vec<(RequestId, i64)>) -> Vec<(String, i64)> {
        list.into_iter()
            .map(|(request, at)| (request.as_str().to_owned(), at))
            .collect()
    }

    #[tokio::test]
    async fn the_ledger_tells_a_retry_from_another_command_and_keeps_what_is_open() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&one, "c-2", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let (chat, other) = (id::<ChatId>("c-1"), id::<ChatId>("c-2"));
        assert_eq!(
            record(&one, "r-1", &chat, b"h1", NOW + 2).await,
            Recorded::New
        );
        assert_eq!(
            record(&one, "r-2", &chat, b"h2", NOW + 1).await,
            Recorded::New
        );
        let same = Recorded::Same { open: true };
        assert_eq!(record(&one, "r-1", &chat, b"h1", NOW + 9).await, same);
        assert_eq!(
            record(&one, "r-1", &chat, b"hx", NOW).await,
            Recorded::Conflict
        );
        assert_eq!(
            record(&one, "r-1", &other, b"h1", NOW).await,
            Recorded::Conflict
        );
        let open = opened(one.open_requests(&chat).await.unwrap());
        let expected = [("r-c-1", NOW), ("r-2", NOW + 1), ("r-1", NOW + 2)];
        assert_eq!(open, expected.map(|(request, at)| (request.to_owned(), at)));

        one.end_request(&id("r-1")).await.unwrap();
        assert_eq!(
            record(&one, "r-1", &chat, b"h1", NOW).await,
            Recorded::Same { open: false }
        );
        let open = opened(one.open_requests(&chat).await.unwrap());
        assert_eq!(open.len(), 2);
        assert_eq!(opened(two.open_requests(&chat).await.unwrap()).len(), 1);
        let member = member_id("m2");
        let elsewhere = NewRequest {
            id: &id("r-1"),
            chat: &chat,
            member: &member,
            action_hash: b"h1",
            at: NOW,
        };
        assert_eq!(two.record_request(&elsewhere).await.unwrap(), Recorded::New);
    }
}
