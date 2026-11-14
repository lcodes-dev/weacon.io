use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{ChatEntry, ChatEvent, ChatId, ChatState, Outcome, RequestId, code};

use super::super::changes::Versioned;
use super::super::{Workspace, failed, stored, unreadable};
use super::{chat_not_found, count, number, unstorable};

/// The most bytes of chat events a workspace keeps: 512 MiB.
pub(crate) const CHAT_QUOTA: i64 = 512 << 20;

/// An event to store, with its place in its machine's log, when it came from there.
#[derive(Debug)]
pub(crate) struct Incoming<'a> {
    pub(crate) core_seq: Option<u64>,
    pub(crate) event: &'a ChatEvent,
    pub(crate) at: i64,
}

impl Workspace {
    /// Stores `events` after the chat's last, in order, and gives the chat's seq after them.
    /// Those from its machine at or before the last one stored are replays and are skipped.
    /// Events that would take the workspace past [`CHAT_QUOTA`] make the chat `full` and are
    /// `chat.quota_full`, with none of them stored.
    pub(crate) async fn append_events(
        &self,
        chat: &ChatId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let (seq, core_seq) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT seq, core_seq FROM chats WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        .ok_or_else(chat_not_found)?;
        let (rows, seq, core_seq) = numbered(events, seq, core_seq)?;
        if !self.charge_chats(&mut transaction, size(&rows)?).await? {
            drop(transaction);
            self.set_chat_state(chat, ChatState::Full).await?;
            return Err(chat_quota_full());
        }
        self.store(&mut transaction, chat, &rows).await?;
        self.advance(&mut transaction, chat, seq, core_seq).await?;
        transaction.commit().await.map_err(failed)?;
        count(seq)
    }

    // Inserts `rows` and folds them into the chat's latest run and waiting prompts.
    async fn store(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        rows: &[Row<'_>],
    ) -> Result<(), Fault> {
        let mut runs = self.read_runs(&mut *transaction, chat).await?;
        for row in rows {
            sqlx::query(
                "INSERT INTO chat_events (workspace_id, chat_id, seq, core_seq, body, at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .bind(row.seq)
            .bind(row.core_seq)
            .bind(&row.body)
            .bind(row.at)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
            runs.apply(&ChatEntry {
                seq: count(row.seq)?,
                event: row.event.clone(),
            });
        }
        self.keep_runs(&mut *transaction, chat, &runs).await?;
        for request in rows.iter().filter_map(|row| ended(row.event)) {
            sqlx::query(
                "UPDATE chat_requests SET open = 0 WHERE workspace_id = ? AND request_id = ?",
            )
            .bind(self.id.as_str())
            .bind(request.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        let mut turned = rows.iter().filter(|row| turns(row.event)).map(|row| row.at);
        match turned.next_back() {
            Some(at) => self.turned(transaction, chat, at).await,
            None => Ok(()),
        }
    }

    // A turn began or ended at `at`: the chat is updated then, and the feed hears of it.
    async fn turned(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        at: i64,
    ) -> Result<(), Fault> {
        let owner = sqlx::query_scalar::<_, String>(
            "UPDATE chats SET updated_at = ? WHERE workspace_id = ? AND chat_id = ?
             RETURNING owner_member",
        )
        .bind(at)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record_change(transaction, Versioned::Chat(chat), &stored(owner)?)
            .await
    }

    async fn advance(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        seq: i64,
        core_seq: i64,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chats SET seq = ?, core_seq = ? WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(seq)
        .bind(core_seq)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Counts `bytes` more against the workspace's quota; `false` when it has no room for them.
    async fn charge_chats(
        &self,
        transaction: &mut SqliteConnection,
        bytes: i64,
    ) -> Result<bool, Fault> {
        let charged = sqlx::query(
            "UPDATE workspaces SET chat_bytes = chat_bytes + ?2
             WHERE workspace_id = ?1 AND chat_bytes + ?2 <= ?3",
        )
        .bind(self.id.as_str())
        .bind(bytes)
        .bind(CHAT_QUOTA)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(charged.rows_affected() > 0)
    }

    /// The chat's events after `after`, oldest first: at most `limit`, and only as many as
    /// fit `max_bytes`, though always one when there is one.
    pub(crate) async fn events_after(
        &self,
        chat: &ChatId,
        after: u64,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Vec<ChatEntry>, Fault> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT seq, body FROM chat_events WHERE workspace_id = ? AND chat_id = ? AND seq > ?
             ORDER BY seq LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(number(after)?)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?;
        entries(rows, max_bytes)
    }

    /// The chat's latest events before `before`, oldest first, limited as in
    /// [`Self::events_after`].
    pub(crate) async fn events_before(
        &self,
        chat: &ChatId,
        before: u64,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Vec<ChatEntry>, Fault> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT seq, body FROM chat_events WHERE workspace_id = ? AND chat_id = ? AND seq < ?
             ORDER BY seq DESC LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(i64::try_from(before).unwrap_or(i64::MAX))
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?;
        let mut latest = entries(rows, max_bytes)?;
        latest.reverse();
        Ok(latest)
    }
}

struct Row<'a> {
    seq: i64,
    core_seq: Option<i64>,
    event: &'a ChatEvent,
    body: String,
    at: i64,
}

// Numbers the events after `seq`, skipping those from the machine at or before `core_seq`,
// and gives the numbers they end at.
fn numbered<'a>(
    events: &[Incoming<'a>],
    mut seq: i64,
    mut core_seq: i64,
) -> Result<(Vec<Row<'a>>, i64, i64), Fault> {
    let mut rows = Vec::new();
    for incoming in events {
        let from_core = incoming.core_seq.map(number).transpose()?;
        if from_core.is_some_and(|at| at <= core_seq) {
            continue;
        }
        core_seq = from_core.unwrap_or(core_seq);
        seq = seq.checked_add(1).ok_or_else(unreadable)?;
        rows.push(Row {
            seq,
            core_seq: from_core,
            event: incoming.event,
            body: serde_json::to_string(incoming.event).map_err(|_| unstorable())?,
            at: incoming.at,
        });
    }
    Ok((rows, seq, core_seq))
}

/// The request a receipt in `event` ends, when it ends one.
fn ended(event: &ChatEvent) -> Option<&RequestId> {
    let ChatEvent::Receipt { receipt } = event else {
        return None;
    };
    (!matches!(receipt.outcome, Outcome::Accepted)).then_some(&receipt.request)
}

fn turns(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::Prompt { .. }) || ended(event).is_some()
}

fn size(rows: &[Row<'_>]) -> Result<i64, Fault> {
    rows.iter().try_fold(0_i64, |sum, row| {
        let len = i64::try_from(row.body.len()).map_err(|_| unstorable())?;
        Ok(sum.saturating_add(len))
    })
}

// Takes rows in their order while they fit `max_bytes`, and the first one whatever its size.
fn entries(rows: Vec<(i64, String)>, max_bytes: usize) -> Result<Vec<ChatEntry>, Fault> {
    let mut used = 0_usize;
    let mut taken = Vec::new();
    for (seq, body) in rows {
        used = used.saturating_add(body.len());
        if used > max_bytes && !taken.is_empty() {
            break;
        }
        let event = serde_json::from_str(&body).map_err(|_| unreadable())?;
        taken.push(ChatEntry {
            seq: count(seq)?,
            event,
        });
    }
    Ok(taken)
}

pub(crate) fn chat_quota_full() -> Fault {
    Fault::new(
        code!("chat.quota_full"),
        "The workspace has no room left for chats",
    )
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Receipt, WorkspaceChange};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, append, code, id, make, said, seqs};
    use crate::test_support::two_teams;

    #[tokio::test]
    async fn events_are_numbered_by_the_server_and_a_replay_from_the_machine_is_skipped() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let unknown = ChatEvent::Receipt {
            receipt: Receipt {
                request: id("r-c-1"),
                outcome: Outcome::Unknown,
            },
        };

        let first = [(Some(2), said("a")), (Some(5), said("b")), (None, unknown)];
        assert_eq!(append(&one, "c-1", &first).await.unwrap(), 3);
        let replayed = [
            (Some(5), said("b")),
            (Some(4), said("x")),
            (Some(6), said("c")),
        ];
        assert_eq!(append(&one, "c-1", &replayed).await.unwrap(), 4);
        assert_eq!(append(&one, "c-1", &[]).await.unwrap(), 4);

        let chat = one.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!((chat.seq, chat.core_seq), (4, 6));
        let missing = append(&two, "c-2", &first).await.unwrap_err();
        assert_eq!(code(&missing), "chat.not_found");
    }

    #[tokio::test]
    async fn events_are_read_after_a_place_or_latest_first_before_one() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let events = [
            (None, said("a")),
            (None, said("b")),
            (None, said("c")),
            (None, said("d")),
        ];
        append(&one, "c-1", &events).await.unwrap();

        let all = one
            .events_after(&id("c-1"), 0, 100, usize::MAX)
            .await
            .unwrap();
        assert_eq!(seqs(&all), [1, 2, 3, 4]);
        assert_eq!(all.get(3).map(|entry| &entry.event), Some(&said("d")));
        assert_eq!(
            seqs(
                &one.events_after(&id("c-1"), 2, 1, usize::MAX)
                    .await
                    .unwrap()
            ),
            [3]
        );
        let latest = one.events_before(&id("c-1"), u64::MAX, 2, usize::MAX).await;
        assert_eq!(seqs(&latest.unwrap()), [3, 4]);
        assert_eq!(
            seqs(
                &one.events_before(&id("c-1"), 3, 9, usize::MAX)
                    .await
                    .unwrap()
            ),
            [1, 2]
        );
        assert!(
            two.events_after(&id("c-1"), 0, 100, usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_window_takes_only_what_fits_but_always_one_event() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        let events = [
            (None, said("aaaa")),
            (None, said("bbbb")),
            (None, said("cccc")),
        ];
        append(&one, "c-1", &events).await.unwrap();
        let size = serde_json::to_string(&said("aaaa")).unwrap().len();

        let fits_two = one.events_after(&id("c-1"), 0, 9, 2 * size).await.unwrap();
        assert_eq!(seqs(&fits_two), [1, 2]);
        let fits_none = one.events_after(&id("c-1"), 0, 9, size - 1).await.unwrap();
        assert_eq!(seqs(&fits_none), [1]);
        let latest = one.events_before(&id("c-1"), 9, 9, 2 * size).await.unwrap();
        assert_eq!(seqs(&latest), [2, 3]);
    }

    #[tokio::test]
    async fn events_past_the_quota_fill_the_chat_and_none_of_them_is_stored() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let size = i64::try_from(serde_json::to_string(&said("a")).unwrap().len()).unwrap();
        append(&one, "c-1", &[(Some(1), said("a"))]).await.unwrap();
        let used: i64 =
            sqlx::query_scalar("SELECT chat_bytes FROM workspaces WHERE workspace_id = 'w1'")
                .fetch_one(&one.db.readers)
                .await
                .unwrap();
        assert_eq!(used, size);
        sqlx::query("UPDATE workspaces SET chat_bytes = ? WHERE workspace_id = 'w1'")
            .bind(CHAT_QUOTA - size)
            .execute(&one.db.writer)
            .await
            .unwrap();

        assert_eq!(
            append(&one, "c-1", &[(Some(2), said("a"))]).await.unwrap(),
            2
        );
        let over = append(&one, "c-1", &[(Some(3), said("a"))])
            .await
            .unwrap_err();
        assert_eq!(code(&over), "chat.quota_full");
        let chat = one.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!(
            (chat.state, chat.seq, chat.core_seq),
            (ChatState::Full, 2, 2)
        );
        assert_eq!(
            append(&two, "c-1", &[(Some(1), said("a"))]).await.unwrap(),
            1
        );
    }

    fn receipt(request: &str, outcome: Outcome) -> ChatEvent {
        ChatEvent::Receipt {
            receipt: Receipt {
                request: id(request),
                outcome,
            },
        }
    }

    async fn changed_chats(workspace: &Workspace) -> Vec<WorkspaceChange> {
        let changes = workspace.changes_since(0, 100).await.unwrap();
        let chats = changes.into_iter().map(|update| update.change);
        chats
            .filter(|change| matches!(change, WorkspaceChange::ChatChanged { .. }))
            .collect()
    }

    async fn updated_at(workspace: &Workspace) -> i64 {
        workspace.chats().await.unwrap().first().unwrap().updated_at
    }

    #[tokio::test]
    async fn a_turn_ending_closes_its_request_and_tells_the_feed_and_the_list() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let made = WorkspaceChange::ChatChanged { chat: id("c-1") };
        assert_eq!(changed_chats(&one).await, std::slice::from_ref(&made));

        let accepted = receipt("r-c-1", Outcome::Accepted);
        let events = [(Some(1), accepted), (Some(2), said("working"))];
        append(&one, "c-1", &events).await.unwrap();
        assert_eq!(one.open_requests(&id("c-1")).await.unwrap().len(), 1);
        assert_eq!(
            (changed_chats(&one).await.len(), updated_at(&one).await),
            (1, NOW)
        );

        let done = Incoming {
            core_seq: Some(3),
            event: &receipt("r-c-1", Outcome::Completed),
            at: NOW + 40,
        };
        one.append_events(&id("c-1"), &[done]).await.unwrap();

        assert!(one.open_requests(&id("c-1")).await.unwrap().is_empty());
        assert_eq!(changed_chats(&one).await, [made.clone(), made]);
        assert_eq!(updated_at(&one).await, NOW + 40);
        assert_eq!(two.open_requests(&id("c-1")).await.unwrap().len(), 1);
        assert_eq!(changed_chats(&two).await.len(), 1);
    }
}
