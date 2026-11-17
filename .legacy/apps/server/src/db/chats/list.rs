//! A workspace's list of chats.

use std::collections::HashMap;

use weacon_observability::Fault;
use weacon_protocol::{ChatId, ChatLeg, ChatSummary};

use super::legs::stored_agent;
use super::{stored_run_state, stored_state};
use crate::db::{Workspace, failed, stored};

type SummaryRow = (
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
);

type LegRow = (String, String, String, String, String, Option<String>, bool);

impl Workspace {
    /// The workspace's chats, the latest updated first.
    pub(crate) async fn chats(&self) -> Result<Vec<ChatSummary>, Fault> {
        let mut reader = self.db.readers.begin().await.map_err(failed)?;
        let mut legs = self.listed_legs(&mut reader).await?;
        sqlx::query_as::<_, SummaryRow>(
            "SELECT chat_id, c.title, owner_member, state, updated_at, run_status, t.task_id
             FROM chats AS c LEFT JOIN chat_tasks AS t USING (workspace_id, chat_id)
             WHERE workspace_id = ? AND EXISTS (SELECT 1 FROM changes AS h
                 WHERE h.workspace_id = c.workspace_id AND h.chat_id = c.chat_id)
             ORDER BY updated_at DESC, chat_id",
        )
        .bind(self.id.as_str())
        .fetch_all(&mut *reader)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, title, owner, state, updated_at, run, task)| {
            let id: ChatId = stored(id)?;
            Ok(ChatSummary {
                legs: legs.remove(&id).unwrap_or_default(),
                id,
                title,
                owner: stored(owner)?,
                state: stored_state(&state)?,
                updated_at,
                run: run.as_deref().map(stored_run_state).transpose()?,
                task: task.map(stored).transpose()?,
            })
        })
        .collect()
    }

    // Each chat's legs whose machines took a prompt, in the order they were made.
    async fn listed_legs(
        &self,
        reader: &mut sqlx::SqliteConnection,
    ) -> Result<HashMap<ChatId, Vec<ChatLeg>>, Fault> {
        let rows = sqlx::query_as::<_, LegRow>(
            "SELECT chat_id, member_id, machine_id, machine_name, agent, repo, detached
             FROM chat_legs WHERE workspace_id = ? AND core_seq > 0 ORDER BY rowid",
        )
        .bind(self.id.as_str())
        .fetch_all(reader)
        .await
        .map_err(failed)?;
        let mut legs = HashMap::<ChatId, Vec<ChatLeg>>::new();
        for (chat, member, machine_id, machine, agent, repo, detached) in rows {
            legs.entry(stored(chat)?).or_default().push(ChatLeg {
                member: stored(member)?,
                machine_id: stored(machine_id)?,
                machine: stored(machine)?,
                agent: stored_agent(&agent)?,
                repo: repo.map(stored).transpose()?,
                detached,
            });
        }
        Ok(legs)
    }
}
