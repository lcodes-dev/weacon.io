mod events;
mod ledger;
mod runs;

use weacon_observability::Fault;
use weacon_protocol::{
    ChatId, ChatState, ChatSummary, MachineId, MachineName, MemberId, RepoId, code,
};

pub(crate) use events::{Incoming, chat_quota_full, turns};
pub(crate) use ledger::{NewRequest, Recorded};

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
}

type ChatRow = (String, String, String, i64, i64);
type SummaryRow = (String, String, String, String, String, i64);

impl Workspace {
    /// Makes the chat `chat` with the request that makes it, before that is sent: feeds hear
    /// of it once [`Self::chat_made`] says it was. A chat id or request id already in this
    /// workspace is `db.exists`.
    pub(crate) async fn add_chat(
        &self,
        chat: &NewChat<'_>,
        request: &NewRequest<'_>,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
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
        self.insert_request(&mut transaction, request).await?;
        transaction.commit().await.map_err(failed)
    }

    /// Tells the feed that `owner` made `chat`, once its first prompt reached its machine.
    pub(crate) async fn chat_made(&self, chat: &ChatId, owner: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        self.record_change(&mut transaction, Versioned::Chat(chat), owner)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Takes back `chat`, made for a first prompt its machine never got.
    pub(crate) async fn forget_chat(&self, chat: &ChatId) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        sqlx::query("DELETE FROM chat_requests WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query("DELETE FROM chats WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }

    /// The chat `id`, if this workspace has it.
    pub(crate) async fn chat(&self, id: &ChatId) -> Result<Option<Chat>, Fault> {
        sqlx::query_as::<_, ChatRow>(
            "SELECT owner_member, machine_id, state, seq, core_seq FROM chats
             WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(|(owner, machine, state, seq, core_seq)| {
            Ok(Chat {
                owner: stored(owner)?,
                machine: stored(machine)?,
                state: stored_state(&state)?,
                seq: count(seq)?,
                core_seq: count(core_seq)?,
            })
        })
        .transpose()
    }

    /// Whether `chat` runs on `machine`, and the account that owns the machine is still the
    /// chat's owner here.
    pub(crate) async fn runs_on(&self, chat: &ChatId, machine: &MachineId) -> Result<bool, Fault> {
        let found = sqlx::query(
            "SELECT 1 FROM chats AS c
             JOIN members AS m ON m.workspace_id = c.workspace_id AND m.member_id = c.owner_member
             JOIN machines AS d ON d.machine_id = c.machine_id AND d.account_id = m.account_id
             WHERE c.workspace_id = ? AND c.chat_id = ? AND c.machine_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(machine.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?;
        Ok(found.is_some())
    }

    /// The workspace's chats, the latest updated first.
    pub(crate) async fn chats(&self) -> Result<Vec<ChatSummary>, Fault> {
        sqlx::query_as::<_, SummaryRow>(
            "SELECT chat_id, title, owner_member, machine_name, state, updated_at FROM chats
             WHERE workspace_id = ? ORDER BY updated_at DESC, chat_id",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, title, owner, machine, state, updated_at)| {
            Ok(ChatSummary {
                id: stored(id)?,
                title,
                owner: stored(owner)?,
                machine: stored(machine)?,
                state: stored_state(&state)?,
                updated_at,
            })
        })
        .collect()
    }

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

    /// The open chats on `machine` whose turn may not be over, a run at work or a command no
    /// receipt has ended, each with the last of the machine's events it has.
    pub(crate) async fn unsettled_on(
        &self,
        machine: &MachineId,
    ) -> Result<Vec<(ChatId, u64)>, Fault> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT chat_id, core_seq FROM chats AS c
             WHERE workspace_id = ? AND machine_id = ? AND state = 'open'
                 AND (json_extract(run_status, '$.state') IN ('starting', 'running', 'waiting')
                     OR EXISTS (SELECT 1 FROM chat_requests AS r WHERE r.workspace_id = c.workspace_id
                         AND r.chat_id = c.chat_id AND r.open = 1))
             ORDER BY chat_id",
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

    use weacon_protocol::RunState;

    use super::*;
    use crate::db::test_support::{NOW, append, code, id, make, status};
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
        make(&two, "c-1", "m2", NOW).await.unwrap();
        for chat in ["c-2", "c-3"] {
            one.close(chat, &format!("r-{chat}")).await;
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
    }
}
