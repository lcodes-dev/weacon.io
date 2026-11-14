use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{ChatCommand, ChatId, MemberId};

use super::super::{Workspace, failed, unreadable};
use super::unstorable;

/// A command a member sent a chat, as it is relayed, with `by` stamped.
#[derive(Debug)]
pub(crate) struct NewRequest<'a> {
    pub(crate) command: &'a ChatCommand,
    pub(crate) member: &'a MemberId,
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
        let earlier = sqlx::query_as::<_, (String, i64)>(
            "SELECT command, open FROM chat_requests WHERE workspace_id = ? AND request_id = ?",
        )
        .bind(self.id.as_str())
        .bind(request.command.request.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let recorded = match earlier {
            None => {
                self.insert_request(&mut transaction, request).await?;
                Recorded::New
            }
            Some((earlier, open)) => {
                let earlier = stored_command(&earlier)?;
                let asked = request.command;
                if earlier.chat == asked.chat && earlier.action == asked.action {
                    Recorded::Same { open: open != 0 }
                } else {
                    Recorded::Conflict
                }
            }
        };
        transaction.commit().await.map_err(failed)?;
        Ok(recorded)
    }

    /// Closes `request`, whose receipt ended it.
    pub(crate) async fn end_request(
        &self,
        request: &weacon_protocol::RequestId,
    ) -> Result<(), Fault> {
        sqlx::query("UPDATE chat_requests SET open = 0 WHERE workspace_id = ? AND request_id = ?")
            .bind(self.id.as_str())
            .bind(request.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// The commands to `chat` no receipt has ended yet, oldest first, with when each was sent.
    pub(crate) async fn open_requests(
        &self,
        chat: &ChatId,
    ) -> Result<Vec<(ChatCommand, i64)>, Fault> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT command, sent_at FROM chat_requests
             WHERE workspace_id = ? AND chat_id = ? AND open = 1 ORDER BY sent_at, rowid",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(command, sent_at)| Ok((stored_command(&command)?, sent_at)))
        .collect()
    }

    pub(super) async fn insert_request(
        &self,
        transaction: &mut SqliteConnection,
        request: &NewRequest<'_>,
    ) -> Result<(), Fault> {
        sqlx::query(
            "INSERT INTO chat_requests
                 (workspace_id, request_id, chat_id, member_id, command, sent_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(request.command.request.as_str())
        .bind(request.command.chat.as_str())
        .bind(request.member.as_str())
        .bind(serde_json::to_string(request.command).map_err(|_| unstorable())?)
        .bind(request.at)
        .execute(transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }
}

fn stored_command(json: &str) -> Result<ChatCommand, Fault> {
    serde_json::from_str(json).map_err(|_| unreadable())
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, command, id, make};
    use crate::test_support::{member_id, two_teams};

    async fn record(
        workspace: &Workspace,
        request: &str,
        chat: &str,
        text: &str,
        at: i64,
    ) -> Recorded {
        let (command, member) = (command(request, chat, text), member_id("m1"));
        let new = NewRequest {
            command: &command,
            member: &member,
            at,
        };
        workspace.record_request(&new).await.unwrap()
    }

    fn opened(list: Vec<(ChatCommand, i64)>) -> Vec<(String, i64)> {
        list.into_iter()
            .map(|(command, at)| (command.request.as_str().to_owned(), at))
            .collect()
    }

    #[tokio::test]
    async fn the_ledger_tells_a_retry_from_another_command_and_keeps_what_is_open() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&one, "c-2", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let chat = id::<ChatId>("c-1");
        assert_eq!(
            record(&one, "r-1", "c-1", "Go", NOW + 2).await,
            Recorded::New
        );
        assert_eq!(
            record(&one, "r-2", "c-1", "Stop", NOW + 1).await,
            Recorded::New
        );
        let same = Recorded::Same { open: true };
        assert_eq!(record(&one, "r-1", "c-1", "Go", NOW + 9).await, same);
        assert_eq!(
            record(&one, "r-1", "c-1", "Other", NOW).await,
            Recorded::Conflict
        );
        assert_eq!(
            record(&one, "r-1", "c-2", "Go", NOW).await,
            Recorded::Conflict
        );
        let open = opened(one.open_requests(&chat).await.unwrap());
        let expected = [("r-c-1", NOW), ("r-2", NOW + 1), ("r-1", NOW + 2)];
        assert_eq!(open, expected.map(|(request, at)| (request.to_owned(), at)));

        one.end_request(&id("r-1")).await.unwrap();
        assert_eq!(
            record(&one, "r-1", "c-1", "Go", NOW).await,
            Recorded::Same { open: false }
        );
        let open = opened(one.open_requests(&chat).await.unwrap());
        assert_eq!(open.len(), 2);
        assert_eq!(opened(two.open_requests(&chat).await.unwrap()).len(), 1);
        let (asked, member) = (command("r-1", "c-1", "Go"), member_id("m2"));
        let elsewhere = NewRequest {
            command: &asked,
            member: &member,
            at: NOW,
        };
        assert_eq!(two.record_request(&elsewhere).await.unwrap(), Recorded::New);
    }
}
