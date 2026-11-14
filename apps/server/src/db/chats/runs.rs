use sqlx::SqliteConnection;
use weacon_model::{ChatRuns, Waiting};
use weacon_observability::Fault;
use weacon_protocol::{ChatEntry, ChatId, RunStatus};

use super::super::{Workspace, failed, stored, unreadable};
use super::events::{Place, Read};
use super::{count, number};

/// The most bytes a waiting prompt's place takes in a snapshot: a `u64` and its comma.
const WAITING_BYTES: usize = 21;

/// A chat as one read sees it: the last of its events, its runs and its latest events.
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub(crate) seq: u64,
    pub(crate) runs: ChatRuns,
    pub(crate) events: Vec<ChatEntry>,
}

impl Workspace {
    /// The chat `chat`, if this workspace has it, read in one transaction: at most `limit` of
    /// its latest events, which with its waiting prompts' places fit `max_bytes`, though there
    /// is always one event when there is one.
    pub(crate) async fn snapshot(
        &self,
        chat: &ChatId,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Option<Snapshot>, Fault> {
        let mut reader = self.db.readers.begin().await.map_err(failed)?;
        let seq = sqlx::query_scalar::<_, i64>(
            "SELECT seq FROM chats WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_optional(&mut *reader)
        .await
        .map_err(failed)?;
        let Some(seq) = seq else {
            return Ok(None);
        };
        let runs = self.read_runs(&mut reader, chat).await?;
        let room = max_bytes.saturating_sub(WAITING_BYTES.saturating_mul(runs.waiting.len()));
        let latest = Read {
            chat,
            place: Place::Before(u64::MAX),
            limit,
            max_bytes: room,
        };
        let events = self.read_events(&mut reader, latest).await?;
        Ok(Some(Snapshot {
            seq: count(seq)?,
            runs,
            events,
        }))
    }

    /// The chat's latest run and the prompts that wait for its owner.
    #[cfg(test)]
    pub(crate) async fn chat_runs(&self, chat: &ChatId) -> Result<ChatRuns, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.read_runs(&mut reader, chat).await
    }

    pub(super) async fn read_runs(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
    ) -> Result<ChatRuns, Fault> {
        let status = sqlx::query_scalar::<_, Option<String>>(
            "SELECT run_status FROM chats WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(failed)?
        .flatten();
        let runs = status
            .map(|json| serde_json::from_str::<RunStatus>(&json))
            .transpose()
            .map_err(|_| unreadable())?;
        let waiting = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT run_id, prompt, seq FROM chat_waiting
             WHERE workspace_id = ? AND chat_id = ? ORDER BY seq",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_all(&mut *connection)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(run, prompt, seq)| {
            Ok(Waiting {
                run: stored(run)?,
                prompt: u32::try_from(prompt).map_err(|_| unreadable())?,
                seq: count(seq)?,
            })
        })
        .collect::<Result<_, Fault>>()?;
        Ok(ChatRuns {
            runs: runs.into_iter().collect(),
            waiting,
        })
    }

    /// Keeps `runs` as the chat's latest run and waiting prompts, in place of what was there.
    pub(super) async fn keep_runs(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        runs: &ChatRuns,
    ) -> Result<(), Fault> {
        let latest = runs.runs.last();
        let status = latest
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| unreadable())?;
        sqlx::query(
            "UPDATE chats SET run_id = ?, run_status = ? WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(latest.map(|status| status.run.as_str()))
        .bind(status)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        sqlx::query("DELETE FROM chat_waiting WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        for waits in &runs.waiting {
            sqlx::query(
                "INSERT INTO chat_waiting (workspace_id, chat_id, run_id, prompt, seq)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .bind(waits.run.as_str())
            .bind(waits.prompt)
            .bind(number(waits.seq)?)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::RunState;
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, answers, append, asks, id, make, status};
    use crate::test_support::two_teams;

    fn waits(runs: &ChatRuns) -> Vec<(u32, u64)> {
        runs.waiting
            .iter()
            .map(|waits| (waits.prompt, waits.seq))
            .collect()
    }

    #[tokio::test]
    async fn the_latest_run_and_its_waiting_prompts_last_from_one_append_to_the_next() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let first = [
            (Some(1), status(RunState::Running)),
            (Some(2), asks(1)),
            (Some(3), asks(2)),
        ];
        append(&one, "c-1", &first).await.unwrap();

        let runs = one.chat_runs(&id("c-1")).await.unwrap();
        assert_eq!(
            runs.runs.first().map(|run| run.state),
            Some(RunState::Running)
        );
        assert_eq!(waits(&runs), [(1, 2), (2, 3)]);
        assert_eq!(
            two.chat_runs(&id("c-1")).await.unwrap(),
            ChatRuns::default()
        );

        append(&one, "c-1", &[(Some(4), answers(1))]).await.unwrap();
        assert_eq!(waits(&one.chat_runs(&id("c-1")).await.unwrap()), [(2, 3)]);
        append(&one, "c-1", &[(Some(5), status(RunState::Idle))])
            .await
            .unwrap();

        let runs = one.chat_runs(&id("c-1")).await.unwrap();
        assert_eq!(runs.runs.first().map(|run| run.state), Some(RunState::Idle));
        assert!(runs.waiting.is_empty());
        let run_id: Option<String> =
            sqlx::query_scalar("SELECT run_id FROM chats WHERE workspace_id = 'w1'")
                .fetch_one(&one.db.readers)
                .await
                .unwrap();
        assert_eq!(run_id.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn a_snapshot_leaves_room_for_its_waiting_prompts_places() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        let said = crate::db::test_support::said("a");
        let events = [
            (Some(1), status(RunState::Waiting)),
            (Some(2), asks(1)),
            (Some(3), said.clone()),
        ];
        append(&one, "c-1", &events).await.unwrap();
        let bytes = |event| serde_json::to_string(event).unwrap().len();
        let both = bytes(&asks(1)) + bytes(&said) + WAITING_BYTES;

        let fits = one.snapshot(&id("c-1"), 50, both).await.unwrap().unwrap();
        let short = one
            .snapshot(&id("c-1"), 50, both - 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!((fits.seq, waits(&fits.runs)), (3, vec![(1, 2)]));
        let seqs = |snapshot: &Snapshot| {
            snapshot
                .events
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>()
        };
        assert_eq!(seqs(&fits), [2, 3]);
        assert_eq!(seqs(&short), [3]);
        assert!(one.snapshot(&id("c-9"), 50, both).await.unwrap().is_none());
    }
}
