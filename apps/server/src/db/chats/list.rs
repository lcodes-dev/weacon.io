//! A workspace's list of chats.

use weacon_observability::Fault;
use weacon_protocol::ChatSummary;

use super::{stored_run_state, stored_state};
use crate::db::{Workspace, failed, stored};

type SummaryRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    String,
    Option<String>,
);

impl Workspace {
    /// The workspace's chats, the latest updated first.
    pub(crate) async fn chats(&self) -> Result<Vec<ChatSummary>, Fault> {
        sqlx::query_as::<_, SummaryRow>(
            "SELECT chat_id, c.title, owner_member, machine_name, state, updated_at, run_status,
                 machine_id, t.task_id
             FROM chats AS c LEFT JOIN chat_tasks AS t USING (workspace_id, chat_id)
             WHERE workspace_id = ? AND EXISTS (SELECT 1 FROM changes AS h
                 WHERE h.workspace_id = c.workspace_id AND h.chat_id = c.chat_id)
             ORDER BY updated_at DESC, chat_id",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(
            |(id, title, owner, machine, state, updated_at, run, machine_id, task)| {
                Ok(ChatSummary {
                    id: stored(id)?,
                    title,
                    owner: stored(owner)?,
                    machine: stored(machine)?,
                    state: stored_state(&state)?,
                    updated_at,
                    run: run.as_deref().map(stored_run_state).transpose()?.flatten(),
                    machine_id: stored(machine_id)?,
                    task: task.map(stored).transpose()?,
                })
            },
        )
        .collect()
    }
}
