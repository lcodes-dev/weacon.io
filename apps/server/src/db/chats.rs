mod events;
mod ledger;
mod runs;

use weacon_observability::Fault;
use weacon_protocol::{
    ChatId, ChatState, ChatSummary, MachineId, MachineName, MemberId, RepoId, code,
};

#[cfg(test)]
pub(crate) use events::Incoming;
pub(crate) use ledger::NewRequest;

use super::changes::Versioned;
use super::{Workspace, failed, stored, unreadable};

/// A chat as its first prompt makes it.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a chat is made when the relay sends its first prompt"
    )
)]
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a chat is made when the relay sends its first prompt"
        )
    )]
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a chat is made when the relay sends its first prompt"
        )
    )]
    pub(crate) async fn chat_made(&self, chat: &ChatId, owner: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        self.record_change(&mut transaction, Versioned::Chat(chat), owner)
            .await?;
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

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the relay detaches a chat whose machine was unpaired"
        )
    )]
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

    use super::*;
    use crate::db::test_support::{NOW, code, id, make};
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
}
