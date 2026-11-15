mod events;
mod ledger;
mod list;
mod runs;
mod task;

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatId, ChatState, ChatTask, MachineId, MachineName, MemberId, RepoId, RunState, RunStatus,
    code,
};

pub(super) use events::Row;
pub(crate) use events::{Incoming, chat_quota_full, turns};
pub(crate) use ledger::{NewRequest, Recorded};
pub(crate) use runs::Snapshot;

use super::changes::Versioned;
use super::{Workspace, failed, stored, unreadable};

/// A chat as its first prompt makes it.
#[derive(Debug)]
pub(crate) struct NewChat<'a> {
    pub(crate) id: &'a ChatId,
    pub(crate) owner: &'a MemberId,
    pub(crate) title: &'a str,
    pub(crate) repo: Option<&'a RepoId>,
    /// Kept without a reference: an unpaired machine leaves its chats readable.
    pub(crate) machine_id: &'a MachineId,
    pub(crate) machine: &'a MachineName,
    /// The task of this workspace it is for, with its title as it is now.
    pub(crate) task: Option<&'a ChatTask>,
    pub(crate) at: i64,
}

/// What the server knows of a chat beyond its summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chat {
    pub(crate) owner: MemberId,
    pub(crate) machine: MachineId,
    pub(crate) state: ChatState,
    pub(crate) seq: u64,
    /// The last of its machine's events it has.
    pub(crate) core_seq: u64,
    /// Whether its first prompt reached its machine, which is when the feed heard of it.
    pub(crate) made: bool,
    /// The task it was made for, with its title as it then was.
    pub(crate) task: Option<ChatTask>,
}

type ChatRow = (
    String,
    String,
    String,
    i64,
    i64,
    bool,
    Option<String>,
    Option<String>,
);

impl Workspace {
    /// Makes the chat `chat` with the request that makes it, before that is sent: feeds hear
    /// of it once [`Self::chat_made`] says it was. A chat id or request id already in this
    /// workspace is `db.exists`, and a machine its owner doesn't own `machine.not_found`.
    pub(crate) async fn add_chat(
        &self,
        chat: &NewChat<'_>,
        request: &NewRequest<'_>,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "INSERT INTO chats (workspace_id, chat_id, owner_member, title, repo, machine_id,
                 machine_name, state, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'open', ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.id.as_str())
        .bind(chat.owner.as_str())
        .bind(chat.title)
        .bind(chat.repo.map(RepoId::as_str))
        .bind(chat.machine_id.as_str())
        .bind(chat.machine.as_str())
        .bind(chat.at)
        .bind(chat.at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        // Checked as it is written, so a machine unpaired meanwhile gets no chat.
        let owned = self.runs_on_in(&mut transaction, chat.id, chat.machine_id);
        if !owned.await? {
            return Err(machine_not_found());
        }
        if let Some(task) = chat.task {
            self.link_task(&mut transaction, chat.id, task).await?;
        }
        self.insert_request(&mut transaction, request).await?;
        transaction.commit().await.map_err(failed)
    }

    /// Tells the feed that `owner` made `chat`, once its first prompt reached its machine, and
    /// indexes it; gives whether there still was such a chat.
    pub(crate) async fn chat_made(&self, chat: &ChatId, owner: &MemberId) -> Result<bool, Fault> {
        let mut transaction = self.db.write().await?;
        let found = sqlx::query_as::<_, (String, i64)>(
            "SELECT title, created_at FROM chats WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let Some((title, at)) = found else {
            return Ok(false);
        };
        self.index_chat(&mut transaction, chat, &title, at).await?;
        self.record_change(&mut transaction, Versioned::Chat(chat), owner)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(true)
    }

    /// Takes back `chat`, made for a first prompt its machine never got, unless it was made
    /// meanwhile.
    pub(crate) async fn forget_chat(&self, chat: &ChatId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        if self.made(&mut transaction, chat).await? {
            return Ok(());
        }
        sqlx::query("DELETE FROM chat_requests WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.unlink_task(&mut transaction, chat).await?;
        sqlx::query("DELETE FROM chats WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }

    async fn made(&self, connection: &mut SqliteConnection, chat: &ChatId) -> Result<bool, Fault> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM changes WHERE workspace_id = ? AND chat_id = ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_one(connection)
        .await
        .map_err(failed)
    }

    /// The chat `id`, if this workspace has it.
    pub(crate) async fn chat(&self, id: &ChatId) -> Result<Option<Chat>, Fault> {
        sqlx::query_as::<_, ChatRow>(
            "SELECT owner_member, machine_id, state, seq, core_seq, EXISTS (SELECT 1 FROM changes
                 AS h WHERE h.workspace_id = c.workspace_id AND h.chat_id = c.chat_id),
                 t.task_id, t.title
             FROM chats AS c LEFT JOIN chat_tasks AS t USING (workspace_id, chat_id)
             WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(
            |(owner, machine, state, seq, core_seq, made, task, title)| {
                Ok(Chat {
                    owner: stored(owner)?,
                    machine: stored(machine)?,
                    state: stored_state(&state)?,
                    seq: count(seq)?,
                    core_seq: count(core_seq)?,
                    made,
                    task: task::stored_task(task, title)?,
                })
            },
        )
        .transpose()
    }

    /// Whether `chat` runs on `machine`, and the account that owns the machine is still the
    /// chat's owner here.
    pub(crate) async fn runs_on(&self, chat: &ChatId, machine: &MachineId) -> Result<bool, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.runs_on_in(&mut reader, chat, machine).await
    }

    /// [`Self::runs_on`], read through `connection`.
    async fn runs_on_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
    ) -> Result<bool, Fault> {
        let found = sqlx::query(
            "SELECT 1 FROM chats AS c
             JOIN members AS m ON m.workspace_id = c.workspace_id AND m.member_id = c.owner_member
             JOIN machines AS d ON d.machine_id = c.machine_id AND d.account_id = m.account_id
             WHERE c.workspace_id = ? AND c.chat_id = ? AND c.machine_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(machine.as_str())
        .fetch_optional(connection)
        .await
        .map_err(failed)?;
        Ok(found.is_some())
    }

    /// [`Self::append`] for events from no machine in particular.
    #[cfg(test)]
    pub(crate) async fn append_events(
        &self,
        chat: &ChatId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let transaction = self.db.write().await?;
        self.append(transaction, chat, events).await
    }

    /// [`Self::append`] for events `machine` sent, stored only while `chat` runs on it
    /// and its owner still owns it, checked in the transaction that stores them; otherwise
    /// `chat.not_found`.
    pub(crate) async fn append_from(
        &self,
        machine: &MachineId,
        chat: &ChatId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let mut transaction = self.db.write().await?;
        if !self.runs_on_in(&mut transaction, chat, machine).await? {
            return Err(chat_not_found());
        }
        self.append(transaction, chat, events).await
    }

    /// Detaches `chat`, whose machine is no longer its owner's, and tells the feed, as `by`.
    pub(crate) async fn detach_chat(&self, chat: &ChatId, by: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        sqlx::query("UPDATE chats SET state = ? WHERE workspace_id = ? AND chat_id = ?")
            .bind(state_text(ChatState::Detached)?)
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.record_change(&mut transaction, Versioned::Chat(chat), by)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    #[cfg(test)]
    pub(crate) async fn set_chat_state(&self, id: &ChatId, state: ChatState) -> Result<(), Fault> {
        sqlx::query("UPDATE chats SET state = ? WHERE workspace_id = ? AND chat_id = ?")
            .bind(state_text(state)?)
            .bind(self.id.as_str())
            .bind(id.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// The open chats on `machine`, whose owner still owns it, whose first prompt never
    /// reached it, each with its owner.
    pub(crate) async fn unmade_on(
        &self,
        machine: &MachineId,
    ) -> Result<Vec<(ChatId, MemberId)>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT c.chat_id, c.owner_member FROM chats AS c
             JOIN members AS m ON m.workspace_id = c.workspace_id AND m.member_id = c.owner_member
             JOIN machines AS d ON d.machine_id = c.machine_id AND d.account_id = m.account_id
             WHERE c.workspace_id = ? AND c.machine_id = ? AND c.state = 'open'
                 AND NOT EXISTS (SELECT 1 FROM changes AS h WHERE h.workspace_id = c.workspace_id
                     AND h.chat_id = c.chat_id)
             ORDER BY c.chat_id",
        )
        .bind(self.id.as_str())
        .bind(machine.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(chat, owner)| Ok((stored(chat)?, stored(owner)?)))
        .collect()
    }

    /// The open chats made on `machine`, whose owner still owns it, and whose turn may not be
    /// over, a run at work or a command no receipt has ended, each with the last of its events
    /// it has.
    pub(crate) async fn unsettled_on(
        &self,
        machine: &MachineId,
    ) -> Result<Vec<(ChatId, u64)>, Fault> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT c.chat_id, c.core_seq FROM chats AS c
             JOIN members AS m ON m.workspace_id = c.workspace_id AND m.member_id = c.owner_member
             JOIN machines AS d ON d.machine_id = c.machine_id AND d.account_id = m.account_id
             WHERE c.workspace_id = ? AND c.machine_id = ? AND c.state = 'open'
                 AND EXISTS (SELECT 1 FROM changes AS h WHERE h.workspace_id = c.workspace_id
                     AND h.chat_id = c.chat_id)
                 AND (json_extract(c.run_status, '$.state') IN ('starting', 'running', 'waiting')
                     OR EXISTS (SELECT 1 FROM chat_requests AS r WHERE r.workspace_id = c.workspace_id
                         AND r.chat_id = c.chat_id AND r.open = 1))
             ORDER BY c.chat_id",
        )
        .bind(self.id.as_str())
        .bind(machine.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(chat, core_seq)| Ok((stored(chat)?, count(core_seq)?)))
        .collect()
    }
}

pub(crate) fn machine_not_found() -> Fault {
    Fault::new(code!("machine.not_found"), "There is no such machine")
}

pub(crate) fn chat_not_found() -> Fault {
    Fault::new(code!("chat.not_found"), "There is no such chat")
}

fn unstorable() -> Fault {
    Fault::new(code!("chat.unstorable"), "The chat's event can't be stored")
}

fn count(stored: i64) -> Result<u64, Fault> {
    u64::try_from(stored).map_err(|_| unreadable())
}

fn number(seq: u64) -> Result<i64, Fault> {
    i64::try_from(seq).map_err(|_| unstorable())
}

fn state_text(state: ChatState) -> Result<&'static str, Fault> {
    match state {
        ChatState::Open => Ok("open"),
        ChatState::Full => Ok("full"),
        ChatState::Detached => Ok("detached"),
        ChatState::Newer => Err(unstorable()),
    }
}

// `status` is a stored `RunStatus` in JSON; a state from a newer core is never sent on.
fn stored_run_state(status: &str) -> Result<Option<RunState>, Fault> {
    let status = serde_json::from_str::<RunStatus>(status).map_err(|_| unreadable())?;
    Ok(Some(status.state).filter(|state| *state != RunState::Newer))
}

fn stored_state(text: &str) -> Result<ChatState, Fault> {
    match text {
        "open" => Ok(ChatState::Open),
        "full" => Ok(ChatState::Full),
        "detached" => Ok(ChatState::Detached),
        _ => Err(unreadable()),
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use weacon_protocol::{ChatSummary, RunState};

    use super::*;
    use crate::db::test_support::{NOW, append, begin, code, command, id, make, status};
    use crate::test_support::{member_id, two_teams};

    #[tokio::test]
    async fn a_chat_is_listed_in_its_own_workspace_only_and_its_id_is_its_workspaces() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&one, "c-2", "m3", NOW + 5).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();

        let listed = one.chats().await.unwrap();
        let summary = ChatSummary {
            id: id("c-2"),
            title: "Fix the build".to_owned(),
            owner: member_id("m3"),
            machine: id("Ada's laptop"),
            state: ChatState::Open,
            updated_at: NOW + 5,
            run: None,
            machine_id: Some(id("mc-1")),
            task: None,
        };
        assert_eq!(listed.first(), Some(&summary));
        assert_eq!(ids(&listed), ["c-2", "c-1"]);
        assert_eq!(ids(&two.chats().await.unwrap()), ["c-1"]);
        let again = make(&one, "c-1", "m1", NOW).await.unwrap_err();
        assert_eq!(code(&again), "db.exists");
        let stranger = make(&one, "c-3", "m2", NOW).await.unwrap_err();
        assert_eq!(code(&stranger), "db.failed");
        let chat = Chat {
            owner: member_id("m1"),
            machine: id("mc-1"),
            state: ChatState::Open,
            seq: 0,
            core_seq: 0,
            made: true,
            task: None,
        };
        assert_eq!(one.chat(&id("c-1")).await.unwrap(), Some(chat));
        assert_eq!(two.chat(&id("c-2")).await.unwrap(), None);
        one.set_chat_state(&id("c-1"), ChatState::Detached)
            .await
            .unwrap();
        let state = one.chat(&id("c-1")).await.unwrap().map(|chat| chat.state);
        assert_eq!(state, Some(ChatState::Detached));
        assert_eq!(
            two.chat(&id("c-1")).await.unwrap().unwrap().state,
            ChatState::Open
        );
        let newer = one.set_chat_state(&id("c-1"), ChatState::Newer).await;
        assert_eq!(code(&newer.unwrap_err()), "chat.unstorable");
    }

    #[test]
    fn a_stored_run_gives_its_state_but_never_one_from_a_newer_core() {
        let status = |state: &str| format!(r#"{{"run":"run-1","state":"{state}"}}"#);

        assert_eq!(
            stored_run_state(&status("waiting")).unwrap(),
            Some(RunState::Waiting)
        );
        assert_eq!(stored_run_state(&status("paused")).unwrap(), None);
        assert_eq!(code(&stored_run_state("{").unwrap_err()), "db.unreadable");
    }

    fn ids(list: &[ChatSummary]) -> Vec<&str> {
        list.iter().map(|chat| chat.id.as_str()).collect()
    }

    #[tokio::test]
    async fn a_machine_follows_again_only_its_open_chats_whose_turn_may_not_be_over() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        for chat in ["c-1", "c-2", "c-3", "c-4", "c-5"] {
            make(&one, chat, "m1", NOW).await.unwrap();
        }
        // Its first prompt never reached the machine.
        begin(&one, "c-6", "m1", NOW).await.unwrap();
        // On mc-1 too, but mc-1 isn't Bo's machine.
        make(&one, "c-7", "m3", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;
        for chat in ["c-2", "c-3"] {
            one.close_request(chat, &format!("r-{chat}")).await;
        }
        let running = status(RunState::Running);
        let idle = status(RunState::Idle);
        append(&one, "c-2", &[(Some(4), idle)]).await.unwrap();
        append(&one, "c-3", &[(Some(7), running)]).await.unwrap();
        one.set_chat_state(&id("c-4"), ChatState::Detached)
            .await
            .unwrap();
        sqlx::query("UPDATE chats SET machine_id = 'mc-2' WHERE chat_id = 'c-5'")
            .execute(&one.db.writer)
            .await
            .unwrap();

        let unsettled = one.unsettled_on(&id("mc-1")).await.unwrap();
        let ids: Vec<_> = unsettled
            .iter()
            .map(|(chat, core_seq)| (chat.as_str(), *core_seq))
            .collect();
        assert_eq!(ids, [("c-1", 0), ("c-3", 7)]);
        assert_eq!(two.unsettled_on(&id("mc-1")).await.unwrap().len(), 1);
        let unmade = one.unmade_on(&id("mc-1")).await.unwrap();
        assert_eq!(unmade, [(id("c-6"), member_id("m1"))]);
        assert!(two.unmade_on(&id("mc-1")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_chat_is_made_only_on_a_machine_its_owner_still_owns() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        one.pair_for("m1", "mc-1").await;
        let (chat, owner, name) = (id("c-1"), member_id("m1"), id("Ada's laptop"));
        let asked = command("r-1", "c-1", "Fix the build");
        let request = NewRequest {
            command: &asked,
            member: &owner,
            at: NOW,
        };
        let on = |machine_id| NewChat {
            id: &chat,
            owner: &owner,
            title: "Fix the build",
            repo: None,
            machine_id,
            machine: &name,
            at: NOW,
            task: None,
        };
        // Unpaired since its owner chose it.
        let (theirs, ours) = (id::<MachineId>("mc-2"), id::<MachineId>("mc-1"));
        let unpaired = one.add_chat(&on(&theirs), &request).await;

        assert_eq!(code(&unpaired.unwrap_err()), "machine.not_found");
        assert_eq!(one.chat(&chat).await.unwrap(), None);
        assert!(!one.has_request(&id("r-1")).await.unwrap());
        one.add_chat(&on(&ours), &request).await.unwrap();
        assert!(one.chat(&chat).await.unwrap().is_some());
    }

    async fn indexed(workspace: &Workspace, chat: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM search_rows AS r JOIN search AS s ON s.rowid = r.row
             WHERE r.workspace_id = ? AND r.kind = 'chat' AND r.item = ?",
        )
        .bind(workspace.id.as_str())
        .bind(chat)
        .fetch_one(&workspace.db.readers)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_chat_taken_back_leaves_nothing_behind_and_a_made_one_is_kept() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        begin(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        make(&one, "c-2", "m1", NOW).await.unwrap();
        // Not made, so neither listed nor found yet.
        assert_eq!(indexed(&one, "c-1").await, 0);
        assert_eq!(ids(&one.chats().await.unwrap()), ["c-2"]);

        one.forget_chat(&id("c-1")).await.unwrap();
        one.forget_chat(&id("c-2")).await.unwrap();

        assert_eq!(one.chat(&id("c-1")).await.unwrap(), None);
        assert!(!one.has_request(&id("r-c-1")).await.unwrap());
        assert!(!one.chat_made(&id("c-1"), &member_id("m1")).await.unwrap());
        assert_eq!(ids(&one.chats().await.unwrap()), ["c-2"]);
        assert_eq!(indexed(&two, "c-1").await, 1);
        assert!(one.chat(&id("c-2")).await.unwrap().is_some());
        assert!(one.has_request(&id("r-c-2")).await.unwrap());
        assert_eq!(indexed(&one, "c-2").await, 1);
    }
}
