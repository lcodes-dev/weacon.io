//! Each member's own agent in a chat: the machine, repository and agent their first prompt
//! there fixed, and the last of that machine's events the chat has.

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, ChatId, MachineId, MachineName, MemberId, RepoId, RunId};

use super::super::changes::Versioned;
use super::{count, machine_not_found};
use crate::db::machine::SHARER;
use crate::db::{Workspace, failed, stored, unreadable};

/// How long a call holds a chat's turn before its agent's run has reported: as long as the
/// server waits for the machine's answer to it.
pub(crate) const CALL_SECS: i64 = 30;

/// A leg as a member's first prompt in a chat makes it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NewLeg<'a> {
    /// Kept without a reference: an unpaired machine leaves the chat readable.
    pub(crate) machine_id: &'a MachineId,
    pub(crate) machine: &'a MachineName,
    pub(crate) agent: AgentKind,
    pub(crate) repo: Option<&'a RepoId>,
}

/// A member's own agent in a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leg {
    pub(crate) member: MemberId,
    pub(crate) machine: MachineId,
    pub(crate) machine_name: MachineName,
    pub(crate) agent: AgentKind,
    pub(crate) repo: Option<RepoId>,
    /// The last of its machine's events the chat has; none until its first prompt was taken.
    pub(crate) core_seq: u64,
    pub(crate) detached: bool,
    /// The last of the chat's events its agent was given as quotes.
    pub(crate) quoted_seq: u64,
    /// Which of the workspace's legs it is, which tells its machine's runs from any other's.
    pub(crate) number: i64,
    /// The member who owns its machine and shares it with the workspace; none for a machine
    /// that is its member's alone.
    pub(crate) shared_by: Option<MemberId>,
}

impl Leg {
    /// What the chat calls its machine's run `run`: every core numbers its own runs, so two
    /// legs' would share ids. None when the name wouldn't be a run id.
    pub(crate) fn chat_run(&self, run: &RunId) -> Option<RunId> {
        RunId::try_from(format!("{}{}", run.as_str(), self.suffix())).ok()
    }

    /// What its machine calls the chat's run `run`; none for a run of another leg.
    pub(crate) fn own_run(&self, run: &RunId) -> Option<RunId> {
        let own = run.as_str().strip_suffix(&self.suffix())?;
        RunId::try_from(own.to_owned()).ok()
    }

    fn suffix(&self) -> String {
        format!("-l{}", self.number)
    }
}

type LegRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    bool,
    i64,
    i64,
    Option<String>,
);

fn columns() -> String {
    format!(
        "l.member_id, l.machine_id, l.machine_name, l.agent, l.repo, l.core_seq, l.detached,
         l.quoted_seq, l.rowid, {SHARER}"
    )
}

// A leg whose machine still serves its member: their account owns it, or its owner shares it
// with the workspace.
fn served() -> String {
    format!(
        "FROM chat_legs AS l
         JOIN members AS m ON m.workspace_id = l.workspace_id AND m.member_id = l.member_id
         JOIN machines AS d ON d.machine_id = l.machine_id
             AND (d.account_id = m.account_id OR {SHARER} IS NOT NULL)"
    )
}

impl Workspace {
    /// Fixes `member`'s leg of `chat` for their call at `now`, which takes the chat's turn as
    /// [`Self::call`] does, in place of one whose machine never sent an event. One whose machine
    /// did, or that a call of theirs still on its way holds, stays, as `chat.machine_fixed`; a
    /// machine that neither is `member`'s nor is shared with the workspace is
    /// `machine.not_found`, and one that holds another member's leg of the chat
    /// `chat.machine_taken`.
    pub(crate) async fn set_leg(
        &self,
        chat: &ChatId,
        member: &MemberId,
        leg: &NewLeg<'_>,
        now: i64,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "DELETE FROM chat_legs WHERE workspace_id = ?1 AND chat_id = ?2 AND member_id = ?3
                 AND core_seq = 0
                 AND NOT EXISTS (SELECT 1 FROM chats WHERE workspace_id = ?1 AND chat_id = ?2
                     AND called_by = ?3 AND called_at > ?4 - ?5)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .bind(now)
        .bind(CALL_SECS)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if self.leg_in(&mut transaction, chat, member).await?.is_some() {
            return Err(machine_fixed());
        }
        self.put_leg(&mut transaction, chat, member, leg).await?;
        self.call_in(&mut transaction, chat, member, now).await?;
        transaction.commit().await.map_err(failed)
    }

    /// Writes `member`'s leg of `chat`, on a machine that serves them.
    pub(super) async fn put_leg(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        member: &MemberId,
        leg: &NewLeg<'_>,
    ) -> Result<(), Fault> {
        // Checked as it is written, so a machine unpaired meanwhile gets no leg.
        if !self.serves_in(transaction, leg.machine_id, member).await? {
            return Err(machine_not_found());
        }
        // A core keeps one log for a chat, so a machine runs one member's agent in it, whether
        // or not it still serves the member who had it first.
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_legs
                 WHERE workspace_id = ? AND chat_id = ? AND machine_id = ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(leg.machine_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        if taken {
            return Err(machine_taken());
        }
        let agent = leg.agent.name().ok_or_else(super::unstorable)?;
        sqlx::query(
            "INSERT INTO chat_legs (workspace_id, chat_id, member_id, machine_id, machine_name,
                 agent, repo)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .bind(leg.machine_id.as_str())
        .bind(leg.machine.as_str())
        .bind(agent)
        .bind(leg.repo.map(RepoId::as_str))
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    // Whether `machine` serves `member`: it is their own, or shared with the workspace.
    async fn serves_in(
        &self,
        connection: &mut SqliteConnection,
        machine: &MachineId,
        member: &MemberId,
    ) -> Result<bool, Fault> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM members AS m
                 JOIN machines AS d ON d.machine_id = ?3
                 WHERE m.workspace_id = ?1 AND m.member_id = ?2
                     AND (d.account_id = m.account_id OR EXISTS (
                         SELECT 1 FROM shared_machines AS s
                         JOIN members AS o ON o.workspace_id = s.workspace_id
                             AND o.account_id = d.account_id
                         WHERE s.workspace_id = ?1 AND s.machine_id = d.machine_id)))",
        )
        .bind(self.id.as_str())
        .bind(member.as_str())
        .bind(machine.as_str())
        .fetch_one(connection)
        .await
        .map_err(failed)
    }

    /// `member`'s leg of `chat`, whoever owns its machine now.
    pub(crate) async fn leg(&self, chat: &ChatId, member: &MemberId) -> Result<Option<Leg>, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.leg_in(&mut reader, chat, member).await
    }

    async fn leg_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        member: &MemberId,
    ) -> Result<Option<Leg>, Fault> {
        let select = format!(
            "SELECT {} FROM chat_legs AS l
             WHERE l.workspace_id = ? AND l.chat_id = ? AND l.member_id = ?",
            columns()
        );
        sqlx::query_as::<_, LegRow>(sqlx::AssertSqlSafe(select))
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .bind(member.as_str())
            .fetch_optional(connection)
            .await
            .map_err(failed)?
            .map(stored_leg)
            .transpose()
    }

    /// Every member's leg of `chat`, in the order they were made.
    pub(crate) async fn legs(&self, chat: &ChatId) -> Result<Vec<Leg>, Fault> {
        let select = format!(
            "SELECT {} FROM chat_legs AS l WHERE l.workspace_id = ? AND l.chat_id = ?
             ORDER BY l.rowid",
            columns()
        );
        sqlx::query_as::<_, LegRow>(sqlx::AssertSqlSafe(select))
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .fetch_all(&self.db.readers)
            .await
            .map_err(failed)?
            .into_iter()
            .map(stored_leg)
            .collect()
    }

    /// The leg of `chat` that `machine` holds, while the machine still serves the leg's member.
    pub(crate) async fn leg_on(
        &self,
        chat: &ChatId,
        machine: &MachineId,
    ) -> Result<Option<Leg>, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.leg_on_in(&mut reader, chat, machine).await
    }

    /// [`Self::leg_on`], read through `connection`.
    pub(super) async fn leg_on_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
    ) -> Result<Option<Leg>, Fault> {
        let select = format!(
            "SELECT {} {}
             WHERE l.workspace_id = ? AND l.chat_id = ? AND l.machine_id = ?",
            columns(),
            served()
        );
        sqlx::query_as::<_, LegRow>(sqlx::AssertSqlSafe(select))
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .bind(machine.as_str())
            .fetch_optional(connection)
            .await
            .map_err(failed)?
            .map(stored_leg)
            .transpose()
    }

    /// Notes that `member`'s agent in `chat` was given the chat up to `seq` as quotes.
    pub(crate) async fn quoted(
        &self,
        chat: &ChatId,
        member: &MemberId,
        seq: u64,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_legs SET quoted_seq = ?
             WHERE workspace_id = ? AND chat_id = ? AND member_id = ?",
        )
        .bind(super::number(seq)?)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Takes `chat`'s turn for `member`'s call at `now`, unless another member called theirs
    /// less than [`CALL_SECS`] ago and its run hasn't reported yet; whether it was taken.
    pub(crate) async fn call(
        &self,
        chat: &ChatId,
        member: &MemberId,
        now: i64,
    ) -> Result<bool, Fault> {
        let mut writer = self.db.writer.acquire().await.map_err(failed)?;
        self.call_in(&mut writer, chat, member, now).await
    }

    async fn call_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        member: &MemberId,
        now: i64,
    ) -> Result<bool, Fault> {
        let called = sqlx::query(
            "UPDATE chats SET called_by = ?1, called_at = ?2
             WHERE workspace_id = ?3 AND chat_id = ?4
                 AND (called_by IS NULL OR called_by = ?1 OR called_at <= ?2 - ?5)",
        )
        .bind(member.as_str())
        .bind(now)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(CALL_SECS)
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(called.rows_affected() > 0)
    }

    /// Gives back the turn `member`'s call took in `chat`, as when it never reached their
    /// machine.
    pub(crate) async fn uncall(&self, chat: &ChatId, member: &MemberId) -> Result<(), Fault> {
        let mut writer = self.db.writer.acquire().await.map_err(failed)?;
        self.uncall_in(&mut writer, chat, member).await
    }

    /// [`Self::uncall`], written through `connection`.
    pub(super) async fn uncall_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        member: &MemberId,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chats SET called_by = NULL, called_at = NULL
             WHERE workspace_id = ? AND chat_id = ? AND called_by = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Detaches `member`'s leg of `chat`, whose machine is no longer theirs, and tells the feed.
    pub(crate) async fn detach_leg(&self, chat: &ChatId, member: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "UPDATE chat_legs SET detached = 1
             WHERE workspace_id = ? AND chat_id = ? AND member_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record_change(&mut transaction, Versioned::Chat(chat), member)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// The legs `machine` holds in made, open chats, whose members it still serves, each with its
    /// chat and the last of the machine's events the chat has: those of chats with a run at
    /// work, then of the latest used, `most` of them at most.
    pub(crate) async fn legs_on(
        &self,
        machine: &MachineId,
        most: u32,
    ) -> Result<Vec<(ChatId, u64)>, Fault> {
        let select = format!(
            "SELECT c.chat_id, l.core_seq {}
             JOIN chats AS c ON c.workspace_id = l.workspace_id AND c.chat_id = l.chat_id
             WHERE l.workspace_id = ? AND l.machine_id = ? AND l.detached = 0 AND c.state = 'open'
                 AND EXISTS (SELECT 1 FROM changes AS h WHERE h.workspace_id = c.workspace_id
                     AND h.chat_id = c.chat_id)
             ORDER BY COALESCE(json_extract(c.run_status, '$.state')
                     IN ('starting', 'running', 'waiting'), 0) DESC, c.updated_at DESC, c.chat_id
             LIMIT ?",
            served()
        );
        sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(select))
            .bind(self.id.as_str())
            .bind(machine.as_str())
            .bind(most)
            .fetch_all(&self.db.readers)
            .await
            .map_err(failed)?
            .into_iter()
            .map(|(chat, core_seq)| Ok((stored(chat)?, count(core_seq)?)))
            .collect()
    }
}

pub(super) fn stored_leg(row: LegRow) -> Result<Leg, Fault> {
    let (member, machine, machine_name, agent, repo, core_seq, detached, quoted_seq, number, by) =
        row;
    Ok(Leg {
        member: stored(member)?,
        machine: stored(machine)?,
        machine_name: stored(machine_name)?,
        agent: stored_agent(&agent)?,
        repo: repo.map(stored).transpose()?,
        core_seq: count(core_seq)?,
        detached,
        quoted_seq: count(quoted_seq)?,
        number,
        shared_by: by.map(stored).transpose()?,
    })
}

pub(super) fn stored_agent(name: &str) -> Result<AgentKind, Fault> {
    [AgentKind::Claude, AgentKind::Codex]
        .into_iter()
        .find(|kind| kind.name() == Some(name))
        .ok_or_else(unreadable)
}

fn machine_taken() -> Fault {
    Fault::new(
        weacon_protocol::code!("chat.machine_taken"),
        "Another member's agent already runs on this machine in this chat",
    )
}

pub(crate) fn machine_fixed() -> Fault {
    Fault::new(
        weacon_protocol::code!("chat.machine_fixed"),
        "Your agent in a chat stays on the machine it started on",
    )
}

#[cfg(test)]
mod tests {
    use weacon_protocol::RunState;
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Incoming;
    use crate::db::test_support::{NOW, code, id, make, said, status};
    use crate::test_support::{member_id, two_teams};

    #[tokio::test]
    async fn a_call_on_its_way_keeps_its_leg_from_another_call_of_the_same_member() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m3", "mc-3").await;
        one.pair_for("m3", "mc-4").await;
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (name, desk, shelf) = (id("Bo's desk"), id("mc-3"), id("mc-4"));
        let on = |machine_id| NewLeg {
            machine_id,
            machine: &name,
            agent: AgentKind::Codex,
            repo: None,
        };

        one.set_leg(&chat, &bo, &on(&shelf), NOW).await.unwrap();
        let held = one.set_leg(&chat, &bo, &on(&desk), NOW + 1).await;

        assert_eq!(code(&held.unwrap_err()), "chat.machine_fixed");
        let leg = one.leg(&chat, &bo).await.unwrap().unwrap();
        assert_eq!(leg.machine.as_str(), "mc-4");
        // The call took the chat's turn as it fixed the leg, so no one else's starts meanwhile.
        assert!(!one.call(&chat, &member_id("m1"), NOW + 1).await.unwrap());
        // Once the hold has run out with no prompt taken, the choice can change.
        let later = NOW + CALL_SECS;
        one.set_leg(&chat, &bo, &on(&desk), later).await.unwrap();
        assert_eq!(
            one.leg(&chat, &bo).await.unwrap().unwrap().machine.as_str(),
            "mc-3"
        );
    }

    #[tokio::test]
    async fn a_members_leg_is_read_in_its_own_workspace_only_and_stays_once_its_machine_took_a_prompt()
     {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        one.pair_for("m3", "mc-3").await;
        one.pair_for("m3", "mc-4").await;
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (name, desk, shelf) = (id("Bo's desk"), id("mc-3"), id("mc-4"));
        let adas = id("mc-1");
        let on = |machine_id| NewLeg {
            machine_id,
            machine: &name,
            agent: AgentKind::Codex,
            repo: None,
        };

        one.set_leg(&chat, &bo, &on(&shelf), NOW).await.unwrap();
        // Given back with no prompt taken, so another choice takes its place.
        one.uncall(&chat, &bo).await.unwrap();
        one.set_leg(&chat, &bo, &on(&desk), NOW).await.unwrap();
        one.uncall(&chat, &bo).await.unwrap();
        let stolen = one.set_leg(&chat, &bo, &on(&adas), NOW).await;

        assert_eq!(code(&stolen.unwrap_err()), "machine.not_found");
        let leg = one.leg(&chat, &bo).await.unwrap().unwrap();
        assert_eq!(
            (leg.machine.as_str(), leg.agent, leg.core_seq, leg.detached),
            ("mc-3", AgentKind::Codex, 0, false)
        );
        assert_eq!(one.leg_on(&chat, &desk).await.unwrap(), Some(leg));
        assert_eq!(one.leg_on(&chat, &shelf).await.unwrap(), None);
        assert_eq!(two.leg(&chat, &bo).await.unwrap(), None);
        assert_eq!(two.leg_on(&chat, &desk).await.unwrap(), None);
        assert!(one.chats().await.unwrap()[0].legs.is_empty());
        let event = status(RunState::Idle);
        let first = Incoming {
            core_seq: Some(1),
            event: &event,
            at: NOW,
        };
        let stored = one.append_from((&bo, &desk), &chat, &[first]).await;
        assert_eq!(stored.unwrap(), 1);
        let fixed = one.set_leg(&chat, &bo, &on(&shelf), NOW).await;
        assert_eq!(code(&fixed.unwrap_err()), "chat.machine_fixed");
        let listed = one.chats().await.unwrap().remove(0).legs;
        let members: Vec<_> = listed.iter().map(|leg| leg.member.as_str()).collect();
        assert_eq!(members, ["m3"]);
        assert_eq!(one.legs_on(&desk, 9).await.unwrap(), [(chat.clone(), 1)]);
        assert!(two.legs_on(&desk, 9).await.unwrap().is_empty());
        assert!(two.chats().await.unwrap()[0].legs.is_empty());
    }

    #[tokio::test]
    async fn a_machine_holds_one_members_leg_of_a_chat_even_once_it_no_longer_serves_them() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m3", "mc-3").await;
        let (chat, ada, bo) = (id::<ChatId>("c-1"), member_id("m1"), member_id("m3"));
        let (name, desk) = (id("Bo's desk"), id::<MachineId>("mc-3"));
        let on_desk = NewLeg {
            machine_id: &desk,
            machine: &name,
            agent: AgentKind::Codex,
            repo: None,
        };
        let (db, w1) = (one.db.clone(), [id("w1")]);

        let later = NOW + 2 * CALL_SECS;
        let unshared = one.set_leg(&chat, &ada, &on_desk, later).await;
        db.set_shares(&desk, &w1).await.unwrap();
        one.set_leg(&chat, &ada, &on_desk, later).await.unwrap();
        let sharer = one.leg(&chat, &ada).await.unwrap().unwrap().shared_by;
        db.set_shares(&desk, &[]).await.unwrap();
        let second = one.set_leg(&chat, &bo, &on_desk, later).await;
        db.set_shares(&desk, &w1).await.unwrap();

        assert_eq!(code(&unshared.unwrap_err()), "machine.not_found");
        assert_eq!(sharer, Some(bo.clone()));
        assert_eq!(code(&second.unwrap_err()), "chat.machine_taken");
        let held = one.leg_on(&chat, &desk).await.unwrap().unwrap();
        assert_eq!(held.member, ada);
        assert_eq!(one.legs(&chat).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_chat_from_before_legs_keeps_its_machine_as_its_owners_leg() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("old.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1');
             INSERT INTO chats (workspace_id, chat_id, owner_member, title, repo, machine_id,
                 machine_name, state, created_at, updated_at, seq, core_seq)
                 VALUES ('w1', 'c-1', 'm1', 'Fix it', 'site', 'mc-1', 'Laptop', 'open', 6, 6, 2, 7),
                        ('w1', 'c-2', 'm1', 'Gone', NULL, 'mc-2', 'Desk', 'detached', 7, 7, 1, 1);
             INSERT INTO chat_events (workspace_id, chat_id, seq, core_seq, body, at) VALUES
                 ('w1', 'c-1', 1, 3, '{\"kind\":\"prompt\",\"request\":\"r-1\",\"text\":\"Go\",\"agent\":\"codex\"}', 8),
                 ('w1', 'c-1', 2, 7, 'not json', 9),
                 ('w1', 'c-2', 1, 1, '{\"kind\":\"outside_prompt\",\"text\":\"Hi\"}', 9);";
        crate::db::schema::database_before(&path, crate::db::schema::steps::CHAT_LEGS, rows).await;

        let db = crate::db::Db::open(&path).await.unwrap();

        let one = db.workspace(&id("w1")).await.unwrap();
        let ada = member_id("m1");
        let leg = one.leg(&id("c-1"), &ada).await.unwrap().unwrap();
        let kept = Leg {
            member: ada.clone(),
            machine: id("mc-1"),
            machine_name: id("Laptop"),
            agent: AgentKind::Codex,
            repo: Some(id("site")),
            core_seq: 7,
            detached: false,
            quoted_seq: 2,
            number: leg.number,
            shared_by: None,
        };
        assert_eq!(leg, kept);
        let gone = one.leg(&id("c-2"), &ada).await.unwrap().unwrap();
        assert_eq!((gone.agent, gone.detached), (AgentKind::Claude, true));
        let chat = one.chat(&id("c-2")).await.unwrap().unwrap();
        assert_eq!(chat.state, weacon_protocol::ChatState::Open);
        let from: Vec<Option<String>> =
            sqlx::query_scalar("SELECT member_id FROM chat_events ORDER BY chat_id, seq")
                .fetch_all(&db.readers)
                .await
                .unwrap();
        assert_eq!(
            from,
            [
                Some("m1".to_owned()),
                Some("m1".to_owned()),
                Some("m1".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn legs_from_before_machines_keyed_them_keep_their_numbers_and_events_their_machine() {
        let dir = TempDir::new().unwrap();
        let path = dir.join("old.db");
        let rows = "INSERT INTO workspaces (workspace_id, name) VALUES ('w1', 'One');
             INSERT INTO accounts (account_id, email) VALUES ('a1', 'ada@example.com'),
                 ('a2', 'bo@example.com');
             INSERT INTO members (workspace_id, member_id, email, account_id)
                 VALUES ('w1', 'm1', 'ada@example.com', 'a1'), ('w1', 'm2', 'bo@example.com', 'a2');
             INSERT INTO chats (workspace_id, chat_id, owner_member, title, state, created_at,
                 updated_at, seq) VALUES ('w1', 'c-1', 'm1', 'Fix it', 'open', 6, 6, 3);
             INSERT INTO chat_legs (rowid, workspace_id, chat_id, member_id, machine_id,
                 machine_name, agent, repo, core_seq, detached, quoted_seq) VALUES
                 (4, 'w1', 'c-1', 'm1', 'mc-1', 'Laptop', 'codex', 'site', 7, 0, 2),
                 (9, 'w1', 'c-1', 'm2', 'mc-2', 'Desk', 'claude', NULL, 1, 1, 0);
             INSERT INTO chat_events (workspace_id, chat_id, seq, member_id, body, at) VALUES
                 ('w1', 'c-1', 1, 'm1', '{\"kind\":\"outside_prompt\",\"text\":\"Hi\"}', 8),
                 ('w1', 'c-1', 2, NULL, '{\"kind\":\"message\",\"author\":\"m2\",\"text\":\"Hi\"}', 9),
                 ('w1', 'c-1', 3, 'm2', '{\"kind\":\"outside_prompt\",\"text\":\"Hi\"}', 9);";
        let step = crate::db::schema::steps::LEGS_BY_MACHINE;
        crate::db::schema::database_before(&path, step, rows).await;

        let db = crate::db::Db::open(&path).await.unwrap();

        let one = db.workspace(&id("w1")).await.unwrap();
        let legs = one.legs(&id("c-1")).await.unwrap();
        let kept = Leg {
            member: member_id("m1"),
            machine: id("mc-1"),
            machine_name: id("Laptop"),
            agent: AgentKind::Codex,
            repo: Some(id("site")),
            core_seq: 7,
            detached: false,
            quoted_seq: 2,
            number: 4,
            shared_by: None,
        };
        assert_eq!(legs.first(), Some(&kept));
        let bos = &legs[1];
        assert_eq!(
            (bos.member.as_str(), bos.machine.as_str(), bos.number),
            ("m2", "mc-2", 9)
        );
        assert!(bos.detached);
        let from: Vec<Option<String>> =
            sqlx::query_scalar("SELECT machine_id FROM chat_events ORDER BY seq")
                .fetch_all(&db.readers)
                .await
                .unwrap();
        assert_eq!(
            from,
            [Some("mc-1".to_owned()), None, Some("mc-2".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_call_holds_its_chats_turn_until_its_run_reports_or_its_time_is_up() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let (chat, ada, bo) = (id::<ChatId>("c-1"), member_id("m1"), member_id("m3"));

        assert!(one.call(&chat, &ada, NOW).await.unwrap());
        assert!(one.call(&chat, &ada, NOW + 1).await.unwrap());
        assert!(!one.call(&chat, &bo, NOW + CALL_SECS).await.unwrap());
        // Another workspace's chat of the same name is free.
        assert!(two.call(&chat, &member_id("m2"), NOW).await.unwrap());
        let called = one.chat(&chat).await.unwrap().unwrap().called;
        assert_eq!(called, Some((ada.clone(), NOW + 1)));
        one.uncall(&chat, &bo).await.unwrap();
        assert!(!one.call(&chat, &bo, NOW + 2).await.unwrap());
        assert!(one.call(&chat, &bo, NOW + 1 + CALL_SECS).await.unwrap());
        one.uncall(&chat, &bo).await.unwrap();
        assert!(one.call(&chat, &ada, NOW + 3 + CALL_SECS).await.unwrap());
        // Her run at work ends the hold; an earlier run of hers going idle doesn't.
        let reports = [
            (said("Hi"), true),
            (status(RunState::Idle), true),
            (status(RunState::Running), false),
        ];
        for (event, held) in reports {
            let incoming = Incoming {
                core_seq: None,
                event: &event,
                at: NOW,
            };
            one.append_as(&chat, &ada, &[incoming]).await.unwrap();
            let called = one.chat(&chat).await.unwrap().unwrap().called;
            assert_eq!(called.is_some(), held);
        }
    }

    #[tokio::test]
    async fn two_machines_places_in_their_own_logs_dont_collide_in_one_chat() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;
        one.pair_for("m3", "mc-3").await;
        let (chat, ada, bo) = (id::<ChatId>("c-1"), member_id("m1"), member_id("m3"));
        let (name, hers, his) = (id("Bo's desk"), id("mc-1"), id("mc-3"));
        let leg = NewLeg {
            machine_id: &his,
            machine: &name,
            agent: AgentKind::Claude,
            repo: None,
        };
        one.set_leg(&chat, &bo, &leg, NOW).await.unwrap();
        let event = status(RunState::Idle);
        let at = |core_seq| Incoming {
            core_seq: Some(core_seq),
            event: &event,
            at: NOW,
        };

        let stored = [
            one.append_from((&ada, &hers), &chat, &[at(1), at(2)]).await,
            one.append_from((&bo, &his), &chat, &[at(1)]).await,
            // A replay from one machine is skipped, whatever the other sent since.
            one.append_from((&ada, &hers), &chat, &[at(2), at(3)]).await,
            one.append_from((&bo, &his), &chat, &[at(1), at(2)]).await,
        ];

        let seqs: Vec<_> = stored.into_iter().map(Result::unwrap).collect();
        assert_eq!(seqs, [2, 3, 4, 5]);
        // Only the machine that holds a member's leg sends as them.
        let forged = one.append_from((&ada, &his), &chat, &[at(9)]).await;
        assert_eq!(code(&forged.unwrap_err()), "chat.not_found");
    }
}
