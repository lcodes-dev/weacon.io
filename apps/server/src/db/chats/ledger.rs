use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{ChatCommand, ChatId, MemberId, RequestId};

use super::super::{Workspace, failed, unreadable};
use super::unstorable;

/// A command a member sent a chat, as it is relayed, with `by` stamped.
#[derive(Debug)]
pub(crate) struct NewRequest<'a> {
    pub(crate) command: &'a ChatCommand,
    pub(crate) member: &'a MemberId,
    pub(crate) at: i64,
}

/// A command no receipt has ended yet.
#[derive(Debug)]
pub(crate) struct OpenRequest {
    pub(crate) command: ChatCommand,
    /// When it was first sent.
    pub(crate) sent_at: i64,
    /// Whether its machine's core refused it for now when it was last sent.
    pub(crate) refused: bool,
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
    /// Records `request` in the ledger, unless it is there already; a chat the quota filled
    /// meanwhile takes none, as `chat.quota_full`.
    pub(crate) async fn record_request(&self, request: &NewRequest<'_>) -> Result<Recorded, Fault> {
        let mut transaction = self.db.write().await?;
        let full = sqlx::query(
            "SELECT 1 FROM chats WHERE workspace_id = ? AND chat_id = ? AND state = 'full'",
        )
        .bind(self.id.as_str())
        .bind(request.command.chat.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        if full.is_some() {
            return Err(super::chat_quota_full());
        }
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

    /// Whether the ledger has `request`, for any chat.
    #[cfg(test)]
    pub(crate) async fn has_request(&self, request: &RequestId) -> Result<bool, Fault> {
        let found =
            sqlx::query("SELECT 1 FROM chat_requests WHERE workspace_id = ? AND request_id = ?")
                .bind(self.id.as_str())
                .bind(request.as_str())
                .fetch_optional(&self.db.readers)
                .await
                .map_err(failed)?;
        Ok(found.is_some())
    }

    /// Takes `request` out of the ledger: it was recorded but never sent, unless another sender
    /// of it sent it meanwhile.
    pub(crate) async fn forget_request(&self, request: &RequestId) -> Result<(), Fault> {
        sqlx::query(
            "DELETE FROM chat_requests WHERE workspace_id = ? AND request_id = ? AND sent = 0",
        )
        .bind(self.id.as_str())
        .bind(request.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Marks `request` sent to its machine, or not sent after all.
    pub(crate) async fn mark_sent(&self, request: &RequestId, sent: bool) -> Result<(), Fault> {
        sqlx::query("UPDATE chat_requests SET sent = ? WHERE workspace_id = ? AND request_id = ?")
            .bind(sent)
            .bind(self.id.as_str())
            .bind(request.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// Closes `request` to `chat`, which its receipt ended.
    pub(in crate::db) async fn end_request(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        request: &RequestId,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_requests SET open = 0
             WHERE workspace_id = ? AND chat_id = ? AND request_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(request.as_str())
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Marks `request` refused for now by its machine's core, or sent it again.
    pub(crate) async fn mark_refused(
        &self,
        request: &RequestId,
        refused: bool,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_requests SET refused = ? WHERE workspace_id = ? AND request_id = ?",
        )
        .bind(refused)
        .bind(self.id.as_str())
        .bind(request.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// The commands to `chat` no receipt has ended yet, oldest first.
    pub(crate) async fn open_requests(&self, chat: &ChatId) -> Result<Vec<OpenRequest>, Fault> {
        sqlx::query_as::<_, (String, i64, bool)>(
            "SELECT command, sent_at, refused FROM chat_requests
             WHERE workspace_id = ? AND chat_id = ? AND open = 1 ORDER BY sent_at, rowid",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(command, sent_at, refused)| {
            let command = stored_command(&command)?;
            Ok(OpenRequest {
                command,
                sent_at,
                refused,
            })
        })
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
    use crate::db::schema::{database_before, steps};
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

    fn opened(list: Vec<OpenRequest>) -> Vec<(String, i64)> {
        list.into_iter()
            .map(|open| (open.command.request.as_str().to_owned(), open.sent_at))
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

        one.close_request("c-1", "r-1").await;
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

    #[tokio::test]
    async fn a_chat_the_quota_filled_records_no_more_commands() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        one.set_chat_state(&id("c-1"), weacon_protocol::ChatState::Full)
            .await
            .unwrap();
        let (asked, member) = (command("r-2", "c-1", "Go"), member_id("m1"));
        let new = NewRequest {
            command: &asked,
            member: &member,
            at: NOW,
        };

        let refused = one.record_request(&new).await.unwrap_err();

        assert_eq!(refused.code().as_str(), "chat.quota_full");
        assert!(!one.has_request(&id("r-2")).await.unwrap());
        let theirs = member_id("m2");
        let elsewhere = NewRequest {
            member: &theirs,
            ..new
        };
        assert_eq!(two.record_request(&elsewhere).await.unwrap(), Recorded::New);
    }

    #[tokio::test]
    async fn the_commands_stored_before_are_sent_but_for_first_prompts_of_unmade_chats() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("weacon.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO chats (workspace_id, chat_id, owner_member, title, machine_id,
                 machine_name, state, created_at, updated_at) VALUES
                 ('w1', 'c-1', 'm1', 'Made', 'mc-1', 'Laptop', 'open', 0, 0),
                 ('w1', 'c-2', 'm1', 'Unmade', 'mc-1', 'Laptop', 'open', 0, 0);
             INSERT INTO changes (workspace_id, seq, kind, member_id, chat_id)
                 VALUES ('w1', 1, 'chat_changed', 'm1', 'c-1');
             INSERT INTO chat_requests (workspace_id, request_id, chat_id, member_id, command,
                 sent_at) VALUES ('w1', 'r-1', 'c-1', 'm1', '{}', 0),
                 ('w1', 'r-2', 'c-2', 'm1', '{}', 0)";
        database_before(&path, steps::REQUESTS_SENT, rows).await;
        let db = crate::db::Db::open(&path).await.unwrap();
        let one = db.workspace(&id("w1")).await.unwrap();

        one.forget_request(&id("r-1")).await.unwrap();
        one.forget_request(&id("r-2")).await.unwrap();

        assert!(one.has_request(&id("r-1")).await.unwrap());
        assert!(!one.has_request(&id("r-2")).await.unwrap());
    }

    #[tokio::test]
    async fn a_failed_send_never_forgets_a_request_another_sender_sent() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        // The first sender records it; the second finds it and sends it.
        assert_eq!(record(&one, "r-2", "c-1", "Go", NOW).await, Recorded::New);
        let again = record(&one, "r-2", "c-1", "Go", NOW).await;
        assert_eq!(again, Recorded::Same { open: true });
        one.mark_sent(&id("r-2"), true).await.unwrap();

        // The first sender's send failed.
        one.forget_request(&id("r-2")).await.unwrap();

        assert!(one.has_request(&id("r-2")).await.unwrap());
        let open = opened(one.open_requests(&id("c-1")).await.unwrap());
        assert!(open.iter().any(|(request, _)| request == "r-2"), "{open:?}");
    }

    #[tokio::test]
    async fn a_request_never_sent_is_forgotten_and_can_be_made_again() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        assert_eq!(record(&one, "r-1", "c-1", "Go", NOW).await, Recorded::New);

        assert!(one.has_request(&id("r-1")).await.unwrap());
        assert!(!two.has_request(&id("r-1")).await.unwrap());
        one.forget_request(&id("r-1")).await.unwrap();
        two.forget_request(&id("r-c-1")).await.unwrap();

        assert!(!one.has_request(&id("r-1")).await.unwrap());
        assert!(one.has_request(&id("r-c-1")).await.unwrap());
        assert_eq!(
            record(&one, "r-1", "c-1", "Other", NOW).await,
            Recorded::New
        );
    }
}
