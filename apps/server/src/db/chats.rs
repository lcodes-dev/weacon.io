mod events;
mod legs;
mod list;
mod quotes;
mod runs;
mod task;

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{ChatId, ChatState, ChatTask, MemberId, RunState, RunStatus, code};

pub(super) use events::Row;
pub(crate) use events::{Incoming, chat_full, turns};
pub(crate) use legs::{Asked, Leg, NewLeg, detached};
pub(crate) use quotes::Said;
pub(crate) use runs::Snapshot;

use super::changes::Versioned;
use super::{Workspace, failed, stored, unreadable};

/// A chat as its first message or prompt makes it.
#[derive(Debug)]
pub(crate) struct NewChat<'a> {
    pub(crate) id: &'a ChatId,
    pub(crate) owner: &'a MemberId,
    pub(crate) title: &'a str,
    /// Its owner's own agent in it, when its first words are a prompt.
    pub(crate) leg: Option<NewLeg<'a>>,
    /// The task of this workspace it is for, with its title as it is now.
    pub(crate) task: Option<&'a ChatTask>,
    pub(crate) at: i64,
}

/// What the server knows of a chat beyond its summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chat {
    pub(crate) owner: MemberId,
    pub(crate) state: ChatState,
    pub(crate) seq: u64,
    /// Whether its first words were taken, which is when the feed heard of it.
    pub(crate) made: bool,
    /// The task it was made for, with its title as it then was.
    pub(crate) task: Option<ChatTask>,
}

type ChatRow = (String, String, i64, bool, Option<String>, Option<String>);

impl Workspace {
    /// Makes the chat `chat` before its first words are stored or sent: feeds hear of it once
    /// [`Self::chat_made`] says it was. A chat id already in this workspace is `db.exists`, and
    /// a leg on a machine its owner doesn't own `machine.not_found`.
    pub(crate) async fn add_chat(&self, chat: &NewChat<'_>) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "INSERT INTO chats (workspace_id, chat_id, owner_member, title, state, created_at,
                 updated_at)
             VALUES (?, ?, ?, ?, 'open', ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.id.as_str())
        .bind(chat.owner.as_str())
        .bind(chat.title)
        .bind(chat.at)
        .bind(chat.at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if let Some(leg) = &chat.leg {
            self.put_leg(&mut transaction, chat.id, chat.owner, leg)
                .await?;
        }
        if let Some(task) = chat.task {
            self.link_task(&mut transaction, chat.id, task).await?;
        }
        transaction.commit().await.map_err(failed)
    }

    /// Tells the feed that `owner` made `chat`, once its first words were taken, and indexes
    /// it; gives whether there still was such a chat.
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

    /// Takes back `chat`, made for first words that were never taken, unless it was made
    /// meanwhile.
    pub(crate) async fn forget_chat(&self, chat: &ChatId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        if self.made(&mut transaction, chat).await? {
            return Ok(());
        }
        self.unlink_task(&mut transaction, chat).await?;
        sqlx::query("DELETE FROM chat_calls WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query("DELETE FROM chat_legs WHERE workspace_id = ? AND chat_id = ?")
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
            "SELECT owner_member, state, seq, EXISTS (SELECT 1 FROM changes
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
        .map(|(owner, state, seq, made, task, title)| {
            Ok(Chat {
                owner: stored(owner)?,
                state: stored_state(&state)?,
                seq: count(seq)?,
                made,
                task: task::stored_task(task, title)?,
            })
        })
        .transpose()
    }

    /// [`Self::append`] for events no machine sent: a person's message, which the server
    /// writes.
    pub(crate) async fn append_events(
        &self,
        chat: &ChatId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let transaction = self.db.write().await?;
        self.append(transaction, chat, None, events).await
    }

    /// [`Self::append`] for events `machine` sent while its leg of `chat` works for `member`,
    /// as those of `by`, stored only while it holds that leg and still serves it, checked in
    /// the transaction that stores them: otherwise `chat.not_found`, or `chat.leg_called` when
    /// the leg works for someone else by now.
    pub(crate) async fn append_from(
        &self,
        (machine, member): (&weacon_protocol::MachineId, &MemberId),
        by: &MemberId,
        chat: &ChatId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let mut transaction = self.db.write().await?;
        let held = self.leg_on_in(&mut transaction, chat, machine).await?;
        if held.ok_or_else(chat_not_found)?.member != *member {
            return Err(leg_called());
        }
        self.append(transaction, chat, Some((by, machine)), events)
            .await
    }

    /// [`Self::append`] for a prompt `machine` sent, as written by `by`: under `taken`, a
    /// request the server took from them, whose record is used up, or else as its owner's own.
    /// From then its leg of `chat` works for `by`. `chat.not_found` when the machine doesn't
    /// serve the leg, and `chat.leg_called` when the record is no longer `by`'s.
    pub(crate) async fn append_prompt(
        &self,
        (machine, by): (&weacon_protocol::MachineId, &MemberId),
        taken: Option<&weacon_protocol::RequestId>,
        chat: &ChatId,
        prompt: Incoming<'_>,
    ) -> Result<u64, Fault> {
        let mut transaction = self.db.write().await?;
        let held = self.leg_on_in(&mut transaction, chat, machine).await?;
        let held = held.ok_or_else(chat_not_found)?;
        // One the chat has already changes nothing.
        if prompt.core_seq.is_some_and(|seq| seq > held.core_seq) {
            if let Some(request) = taken
                && self
                    .take_in(&mut transaction, chat, machine, request)
                    .await?
                    .as_ref()
                    != Some(by)
            {
                return Err(leg_called());
            }
            self.lend_in(&mut transaction, chat, machine, by).await?;
        }
        self.append(transaction, chat, Some((by, machine)), &[prompt])
            .await
    }

    /// [`Self::append`] for events sent by the machine of the leg that works for `member`,
    /// whoever owns it now.
    #[cfg(test)]
    pub(crate) async fn append_as(
        &self,
        chat: &ChatId,
        member: &MemberId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let mut legs = self.legs(chat).await?.into_iter();
        let leg = legs.find(|leg| leg.member == *member);
        let leg = leg.ok_or_else(chat_not_found)?;
        let transaction = self.db.write().await?;
        self.append(transaction, chat, Some((member, &leg.machine)), events)
            .await
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
}

pub(crate) fn machine_not_found() -> Fault {
    Fault::new(code!("machine.not_found"), "There is no such machine")
}

fn leg_called() -> Fault {
    Fault::new(
        code!("chat.leg_called"),
        "The chat's agent there works for someone else now",
    )
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

#[cfg(test)]
fn state_text(state: ChatState) -> Result<&'static str, Fault> {
    match state {
        ChatState::Open => Ok("open"),
        ChatState::Full => Ok("full"),
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
        _ => Err(unreadable()),
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use weacon_protocol::{ChatSummary, RunState};

    use super::*;
    use crate::db::test_support::{NOW, append, begin, code, id, make, status};
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
            // Its owner's machine has taken no prompt yet.
            legs: Vec::new(),
            state: ChatState::Open,
            updated_at: NOW + 5,
            run: None,
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
            state: ChatState::Open,
            seq: 0,
            made: true,
            task: None,
        };
        assert_eq!(one.chat(&id("c-1")).await.unwrap(), Some(chat));
        assert_eq!(two.chat(&id("c-2")).await.unwrap(), None);
        one.set_chat_state(&id("c-1"), ChatState::Full)
            .await
            .unwrap();
        let state = one.chat(&id("c-1")).await.unwrap().map(|chat| chat.state);
        assert_eq!(state, Some(ChatState::Full));
        assert_eq!(
            two.chat(&id("c-1")).await.unwrap().unwrap().state,
            ChatState::Open
        );
        let newer = one.set_chat_state(&id("c-1"), ChatState::Newer).await;
        assert_eq!(code(&newer.unwrap_err()), "chat.unstorable");
    }

    #[tokio::test]
    async fn a_chat_lists_a_leg_once_its_machine_took_a_prompt_in_its_workspace_only() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m3", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();

        append(&one, "c-1", &[(Some(1), status(RunState::Idle))])
            .await
            .unwrap();

        let leg = weacon_protocol::ChatLeg {
            member: member_id("m3"),
            machine_id: id("mc-1"),
            machine: id("Ada's laptop"),
            agent: weacon_protocol::AgentKind::Claude,
            repo: Some(id("site")),
            detached: false,
        };
        assert_eq!(one.chats().await.unwrap()[0].legs, [leg]);
        assert!(two.chats().await.unwrap()[0].legs.is_empty());
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
    async fn a_machine_follows_again_the_legs_it_holds_in_open_chats() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        for chat in ["c-1", "c-2", "c-3", "c-4"] {
            make(&one, chat, "m1", NOW).await.unwrap();
        }
        // Its first prompt never reached the machine.
        begin(&one, "c-6", "m1", NOW).await.unwrap();
        // On mc-1 too, though the last to call it there was Bo.
        make(&one, "c-7", "m3", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;
        append(&one, "c-2", &[(Some(4), status(RunState::Idle))])
            .await
            .unwrap();
        let gone = one.leg(&id("c-3"), &id("mc-1")).await.unwrap().unwrap();
        one.detach_leg(&id("c-3"), &gone).await.unwrap();
        sqlx::query("UPDATE chat_legs SET machine_id = 'mc-2' WHERE chat_id = 'c-4'")
            .execute(&one.db.writer)
            .await
            .unwrap();

        let open = one.legs_on(&id("mc-1"), 9).await.unwrap();
        let ids: Vec<_> = open
            .iter()
            .map(|(chat, core_seq)| (chat.as_str(), *core_seq))
            .collect();
        assert_eq!(ids, [("c-1", 0), ("c-2", 4), ("c-7", 0)]);
        assert_eq!(two.legs_on(&id("mc-1"), 9).await.unwrap().len(), 1);
        // A run at work comes first, so it is followed whatever the bound.
        append(&one, "c-2", &[(Some(5), status(RunState::Running))])
            .await
            .unwrap();
        let first = one.legs_on(&id("mc-1"), 1).await.unwrap();
        assert_eq!(first, [(id("c-2"), 5)]);
    }

    #[tokio::test]
    async fn a_chat_with_no_run_yet_counts_as_at_rest_and_the_latest_used_comes_first() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW - 60).await.unwrap();
        append(&one, "c-1", &[(Some(1), status(RunState::Idle))])
            .await
            .unwrap();
        make(&one, "c-2", "m1", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;

        let first = one.legs_on(&id("mc-1"), 1).await.unwrap();

        assert_eq!(first, [(id("c-2"), 0)]);
    }

    #[tokio::test]
    async fn a_chat_is_made_only_on_a_machine_its_owner_still_owns() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        one.pair_for("m1", "mc-1").await;
        let (chat, owner, name) = (id("c-1"), member_id("m1"), id("Ada's laptop"));
        let on = |machine_id| NewChat {
            id: &chat,
            owner: &owner,
            title: "Fix the build",
            leg: Some(NewLeg {
                machine_id,
                machine: &name,
                agent: weacon_protocol::AgentKind::Claude,
                repo: None,
            }),
            at: NOW,
            task: None,
        };
        // Unpaired since its owner chose it.
        let (theirs, ours) = (
            id::<weacon_protocol::MachineId>("mc-2"),
            id::<weacon_protocol::MachineId>("mc-1"),
        );
        let unpaired = one.add_chat(&on(&theirs)).await;

        assert_eq!(code(&unpaired.unwrap_err()), "machine.not_found");
        assert_eq!(one.chat(&chat).await.unwrap(), None);
        one.add_chat(&on(&ours)).await.unwrap();
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
        assert!(!one.chat_made(&id("c-1"), &member_id("m1")).await.unwrap());
        assert_eq!(ids(&one.chats().await.unwrap()), ["c-2"]);
        assert_eq!(indexed(&two, "c-1").await, 1);
        assert!(one.chat(&id("c-2")).await.unwrap().is_some());
        assert_eq!(indexed(&one, "c-2").await, 1);
    }
}
