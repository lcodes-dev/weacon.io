//! The agents in a chat, one on each machine a member called there: the repository and agent
//! its first prompt fixed, whom it works for, and the last of that machine's events the chat has.

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

/// A leg as the first prompt to a machine in a chat makes it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NewLeg<'a> {
    /// Kept without a reference: an unpaired machine leaves the chat readable.
    pub(crate) machine_id: &'a MachineId,
    pub(crate) machine: &'a MachineName,
    pub(crate) agent: AgentKind,
    pub(crate) repo: Option<&'a RepoId>,
}

/// A machine's agent in a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leg {
    /// Whom it works for: the member who called it last.
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

// A leg whose machine still serves it: one a member of the leg's workspace owns.
const SERVED: &str = "FROM chat_legs AS l
    JOIN machines AS d ON d.machine_id = l.machine_id
    JOIN members AS o ON o.workspace_id = l.workspace_id AND o.account_id = d.account_id";

impl Workspace {
    /// Calls the leg of `chat` on `new`'s machine for `member` at `now`: makes it as `new` says
    /// when the machine has none there, or one that took no prompt and that no call holds,
    /// takes the chat's turn, and has the leg work for `member`. Gives the leg as it was, or
    /// none when another call or leg holds the turn, as [`Self::call`] says. A machine that
    /// neither is `member`'s nor is shared with the workspace is `machine.not_found`, and a
    /// detached leg `chat.detached`.
    pub(crate) async fn call_leg(
        &self,
        chat: &ChatId,
        member: &MemberId,
        new: &NewLeg<'_>,
        now: i64,
    ) -> Result<Option<Leg>, Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "DELETE FROM chat_legs WHERE workspace_id = ?1 AND chat_id = ?2 AND machine_id = ?3
                 AND core_seq = 0
                 AND NOT EXISTS (SELECT 1 FROM chats WHERE workspace_id = ?1 AND chat_id = ?2
                     AND called_at > ?4 - ?5)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(new.machine_id.as_str())
        .bind(now)
        .bind(CALL_SECS)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if !self
            .serves_in(&mut transaction, new.machine_id, member)
            .await?
        {
            return Err(machine_not_found());
        }
        let found = self.leg_in(&mut transaction, chat, new.machine_id).await?;
        let leg = if let Some(leg) = found {
            leg
        } else {
            self.put_leg(&mut transaction, chat, member, new).await?;
            let made = self.leg_in(&mut transaction, chat, new.machine_id).await?;
            made.ok_or_else(machine_not_found)?
        };
        if leg.detached {
            return Err(detached());
        }
        let called = (new.machine_id, member);
        if !self.call_in(&mut transaction, chat, called, now).await? {
            return Ok(None);
        }
        self.lend_in(&mut transaction, chat, new.machine_id, member)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(Some(leg))
    }

    /// Writes the leg of `chat` that `leg` describes, to work for `member`, whom its machine
    /// serves.
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

    /// The leg of `chat` on `machine`, whoever owns the machine now.
    pub(crate) async fn leg(
        &self,
        chat: &ChatId,
        machine: &MachineId,
    ) -> Result<Option<Leg>, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.leg_in(&mut reader, chat, machine).await
    }

    async fn leg_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
    ) -> Result<Option<Leg>, Fault> {
        let select = format!(
            "SELECT {} FROM chat_legs AS l
             WHERE l.workspace_id = ? AND l.chat_id = ? AND l.machine_id = ?",
            columns()
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

    /// Every leg of `chat`, in the order they were made.
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

    /// The leg of `chat` that `machine` holds, while the machine still serves it.
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
            SERVED
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

    /// Notes that the agent on `machine` in `chat` was given the chat up to `seq` as quotes.
    pub(crate) async fn quoted(
        &self,
        chat: &ChatId,
        machine: &MachineId,
        seq: u64,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_legs SET quoted_seq = ?
             WHERE workspace_id = ? AND chat_id = ? AND machine_id = ?",
        )
        .bind(super::number(seq)?)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(machine.as_str())
        .execute(&self.db.writer)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Takes `chat`'s turn for `member`'s call of the leg on `machine` at `now`, unless another
    /// call, of another leg or by another member, was made less than [`CALL_SECS`] ago and its
    /// run hasn't reported yet; whether it was taken.
    pub(crate) async fn call(
        &self,
        chat: &ChatId,
        called: (&MachineId, &MemberId),
        now: i64,
    ) -> Result<bool, Fault> {
        let mut writer = self.db.writer.acquire().await.map_err(failed)?;
        self.call_in(&mut writer, chat, called, now).await
    }

    async fn call_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        (machine, member): (&MachineId, &MemberId),
        now: i64,
    ) -> Result<bool, Fault> {
        let called = sqlx::query(
            "UPDATE chats SET called_by = ?1, called_on = ?2, called_at = ?3
             WHERE workspace_id = ?4 AND chat_id = ?5
                 AND (called_by IS NULL OR called_at <= ?3 - ?6
                     OR (called_by = ?1 AND called_on = ?2))",
        )
        .bind(member.as_str())
        .bind(machine.as_str())
        .bind(now)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(CALL_SECS)
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(called.rows_affected() > 0)
    }

    /// Takes back `member`'s [`Self::call_leg`] of `leg`, which never reached its machine: the
    /// turn is free and the leg works for whom it did.
    pub(crate) async fn uncall_leg(
        &self,
        chat: &ChatId,
        leg: &Leg,
        member: &MemberId,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.uncall_in(&mut transaction, chat, member).await?;
        self.lend_in(&mut transaction, chat, &leg.machine, &leg.member)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    async fn lend_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
        member: &MemberId,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chat_legs SET member_id = ?
             WHERE workspace_id = ? AND chat_id = ? AND machine_id = ?",
        )
        .bind(member.as_str())
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(machine.as_str())
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// [`Self::uncall_in`], on a connection of its own.
    #[cfg(test)]
    pub(crate) async fn uncall(&self, chat: &ChatId, member: &MemberId) -> Result<(), Fault> {
        let mut writer = self.db.writer.acquire().await.map_err(failed)?;
        self.uncall_in(&mut writer, chat, member).await
    }

    /// Gives back the turn `member`'s call took in `chat`, as once its run reports or when it
    /// never reached its machine.
    pub(super) async fn uncall_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        member: &MemberId,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chats SET called_by = NULL, called_on = NULL, called_at = NULL
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

    /// Detaches `leg` of `chat`, whose machine no longer serves it, and tells the feed.
    pub(crate) async fn detach_leg(&self, chat: &ChatId, leg: &Leg) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "UPDATE chat_legs SET detached = 1
             WHERE workspace_id = ? AND chat_id = ? AND machine_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(leg.machine.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record_change(&mut transaction, Versioned::Chat(chat), &leg.member)
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
            "SELECT c.chat_id, l.core_seq {SERVED}
             JOIN chats AS c ON c.workspace_id = l.workspace_id AND c.chat_id = l.chat_id
             WHERE l.workspace_id = ? AND l.machine_id = ? AND l.detached = 0 AND c.state = 'open'
                 AND EXISTS (SELECT 1 FROM changes AS h WHERE h.workspace_id = c.workspace_id
                     AND h.chat_id = c.chat_id)
             ORDER BY COALESCE(json_extract(c.run_status, '$.state')
                     IN ('starting', 'running', 'waiting'), 0) DESC, c.updated_at DESC, c.chat_id
             LIMIT ?"
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

pub(crate) fn detached() -> Fault {
    Fault::new(
        weacon_protocol::code!("chat.detached"),
        "This agent's machine no longer has this chat",
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
    async fn a_call_on_its_way_holds_the_chats_turn_from_a_call_of_any_other_leg() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m3", "mc-3").await;
        one.pair_for("m3", "mc-4").await;
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (name, desk, shelf) = (id("Bo's desk"), id("mc-3"), id("mc-4"));
        let on = |machine_id, agent| NewLeg {
            machine_id,
            machine: &name,
            agent,
            repo: None,
        };
        let codex = AgentKind::Codex;

        let first = one.call_leg(&chat, &bo, &on(&shelf, codex), NOW).await;
        let held = one.call_leg(&chat, &bo, &on(&desk, codex), NOW + 1).await;

        assert_eq!(first.unwrap().unwrap().machine.as_str(), "mc-4");
        assert_eq!(held.unwrap(), None);
        assert_eq!(one.leg(&chat, &desk).await.unwrap(), None);
        // No one else's starts meanwhile either, but the same call may be made again.
        let adas = (&id::<MachineId>("mc-1"), &member_id("m1"));
        assert!(!one.call(&chat, adas, NOW + 1).await.unwrap());
        let again = one.call_leg(&chat, &bo, &on(&shelf, codex), NOW + 1).await;
        assert!(again.unwrap().is_some());
        // Once the hold has run out, another leg is called, and one that took no prompt is
        // made again as its next call says.
        let later = NOW + 1 + CALL_SECS;
        let other = one.call_leg(&chat, &bo, &on(&desk, codex), later).await;
        assert!(other.unwrap().is_some());
        let claude = on(&shelf, AgentKind::Claude);
        let remade = one.call_leg(&chat, &bo, &claude, later + CALL_SECS).await;
        assert_eq!(remade.unwrap().unwrap().agent, AgentKind::Claude);
    }

    #[tokio::test]
    async fn a_leg_is_read_in_its_own_workspace_only_and_stays_as_made_once_its_machine_took_a_prompt()
     {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        one.pair_for("m3", "mc-3").await;
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (name, desk, adas) = (id("Bo's desk"), id("mc-3"), id("mc-1"));
        let on = |machine_id, agent| NewLeg {
            machine_id,
            machine: &name,
            agent,
            repo: None,
        };

        let (codex, stray) = (on(&desk, AgentKind::Codex), on(&adas, AgentKind::Codex));
        let called = one.call_leg(&chat, &bo, &codex, NOW).await;
        called.unwrap().unwrap();
        one.uncall(&chat, &bo).await.unwrap();
        let stolen = one.call_leg(&chat, &bo, &stray, NOW).await;

        assert_eq!(code(&stolen.unwrap_err()), "machine.not_found");
        let leg = one.leg(&chat, &desk).await.unwrap().unwrap();
        assert_eq!(
            (leg.member.as_str(), leg.agent, leg.core_seq, leg.detached),
            ("m3", AgentKind::Codex, 0, false)
        );
        assert_eq!(one.leg_on(&chat, &desk).await.unwrap(), Some(leg));
        assert_eq!(two.leg(&chat, &desk).await.unwrap(), None);
        assert_eq!(two.leg_on(&chat, &desk).await.unwrap(), None);
        assert!(one.chats().await.unwrap()[0].legs.is_empty());
        let event = status(RunState::Idle);
        let first = Incoming {
            core_seq: Some(1),
            event: &event,
            at: NOW,
        };
        let stored = one.append_from(&desk, &chat, &[first]).await;
        assert_eq!(stored.unwrap(), 1);
        let claude = on(&desk, AgentKind::Claude);
        let again = one.call_leg(&chat, &bo, &claude, NOW).await;
        assert_eq!(again.unwrap().unwrap().agent, AgentKind::Codex);
        let kept = one.leg(&chat, &desk).await.unwrap().unwrap();
        assert_eq!((kept.agent, kept.core_seq), (AgentKind::Codex, 1));
        let listed = one.chats().await.unwrap().remove(0).legs;
        let members: Vec<_> = listed.iter().map(|leg| leg.member.as_str()).collect();
        assert_eq!(members, ["m3"]);
        assert_eq!(one.legs_on(&desk, 9).await.unwrap(), [(chat.clone(), 1)]);
        assert!(two.legs_on(&desk, 9).await.unwrap().is_empty());
        assert!(two.chats().await.unwrap()[0].legs.is_empty());
    }

    #[tokio::test]
    async fn a_shared_machines_leg_of_a_chat_is_each_members_to_call_and_its_owners_once_it_isnt() {
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
        let at = |calls: i64| NOW + calls * CALL_SECS;

        let unshared = one.call_leg(&chat, &ada, &on_desk, at(1)).await;
        db.set_shares(&desk, &w1).await.unwrap();
        one.call_leg(&chat, &ada, &on_desk, at(1)).await.unwrap();
        let event = status(RunState::Idle);
        let first = Incoming {
            core_seq: Some(1),
            event: &event,
            at: NOW,
        };
        one.append_from(&desk, &chat, &[first]).await.unwrap();
        let hers = one.leg(&chat, &desk).await.unwrap().unwrap();
        // His call of the same leg waits for hers, as anyone's would.
        let held = one.call_leg(&chat, &bo, &on_desk, at(1)).await;
        let his = one.call_leg(&chat, &bo, &on_desk, at(2)).await;
        db.set_shares(&desk, &[]).await.unwrap();
        let ended = one.call_leg(&chat, &ada, &on_desk, at(3)).await;
        let owners = one.call_leg(&chat, &bo, &on_desk, at(3)).await;

        assert_eq!(code(&unshared.unwrap_err()), "machine.not_found");
        assert_eq!((&hers.member, &hers.shared_by), (&ada, &Some(bo.clone())));
        assert_eq!(held.unwrap(), None);
        assert_eq!(his.unwrap(), Some(hers.clone()));
        assert_eq!(code(&ended.unwrap_err()), "machine.not_found");
        let lent = Leg {
            member: bo.clone(),
            shared_by: None,
            ..hers
        };
        assert_eq!(owners.unwrap(), Some(lent.clone()));
        assert_eq!(one.leg(&chat, &desk).await.unwrap(), Some(lent));
        assert_eq!(one.legs(&chat).await.unwrap().len(), 2);
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
        let leg = one.leg(&id("c-1"), &id("mc-1")).await.unwrap().unwrap();
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
        let gone = one.leg(&id("c-2"), &id("mc-2")).await.unwrap().unwrap();
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

        let (hers, his) = (id::<MachineId>("mc-1"), id::<MachineId>("mc-3"));
        let (adas, bos) = ((&hers, &ada), (&his, &bo));

        assert!(one.call(&chat, adas, NOW).await.unwrap());
        assert!(one.call(&chat, adas, NOW + 1).await.unwrap());
        assert!(!one.call(&chat, bos, NOW + CALL_SECS).await.unwrap());
        // Nor does her own call of another leg, or his of the one she called.
        assert!(!one.call(&chat, (&his, &ada), NOW + 2).await.unwrap());
        assert!(!one.call(&chat, (&hers, &bo), NOW + 2).await.unwrap());
        // Another workspace's chat of the same name is free.
        let theirs = (&hers, &member_id("m2"));
        assert!(two.call(&chat, theirs, NOW).await.unwrap());
        assert_eq!(called(&one).await, Some(("m1".to_owned(), NOW + 1)));
        one.uncall(&chat, &bo).await.unwrap();
        assert!(!one.call(&chat, bos, NOW + 2).await.unwrap());
        assert!(one.call(&chat, bos, NOW + 1 + CALL_SECS).await.unwrap());
        one.uncall(&chat, &bo).await.unwrap();
        assert!(one.call(&chat, adas, NOW + 3 + CALL_SECS).await.unwrap());
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
            assert_eq!(called(&one).await.is_some(), held);
        }
    }

    // Who holds the turn of the workspace's chat c-1, and since when.
    async fn called(workspace: &Workspace) -> Option<(String, i64)> {
        sqlx::query_as(
            "SELECT called_by, called_at FROM chats
             WHERE workspace_id = ? AND chat_id = 'c-1' AND called_by IS NOT NULL",
        )
        .bind(workspace.id.as_str())
        .fetch_optional(&workspace.db.readers)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn two_machines_places_in_their_own_logs_dont_collide_in_one_chat() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;
        one.pair_for("m3", "mc-3").await;
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (name, hers, his) = (id("Bo's desk"), id("mc-1"), id("mc-3"));
        let leg = NewLeg {
            machine_id: &his,
            machine: &name,
            agent: AgentKind::Claude,
            repo: None,
        };
        one.call_leg(&chat, &bo, &leg, NOW).await.unwrap().unwrap();
        let event = status(RunState::Idle);
        let at = |core_seq| Incoming {
            core_seq: Some(core_seq),
            event: &event,
            at: NOW,
        };

        let stored = [
            one.append_from(&hers, &chat, &[at(1), at(2)]).await,
            one.append_from(&his, &chat, &[at(1)]).await,
            // A replay from one machine is skipped, whatever the other sent since.
            one.append_from(&hers, &chat, &[at(2), at(3)]).await,
            one.append_from(&his, &chat, &[at(1), at(2)]).await,
        ];

        let seqs: Vec<_> = stored.into_iter().map(Result::unwrap).collect();
        assert_eq!(seqs, [2, 3, 4, 5]);
        assert_eq!(senders(&one).await, ["m1", "m1", "m3", "m1", "m3"]);
    }

    // Whose each of the workspace's stored events is, in order.
    async fn senders(workspace: &Workspace) -> Vec<String> {
        sqlx::query_scalar("SELECT member_id FROM chat_events WHERE workspace_id = ? ORDER BY seq")
            .bind(workspace.id.as_str())
            .fetch_all(&workspace.db.readers)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_called_leg_works_for_its_caller_until_the_call_is_taken_back() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;
        one.db.set_shares(&id("mc-1"), &[id("w1")]).await.unwrap();
        let (chat, ada, bo) = (id::<ChatId>("c-1"), member_id("m1"), member_id("m3"));
        let (hers, name) = (id::<MachineId>("mc-1"), id("Ada's laptop"));
        let on_hers = NewLeg {
            machine_id: &hers,
            machine: &name,
            agent: AgentKind::Claude,
            repo: None,
        };
        let event = status(RunState::Idle);
        let at = |core_seq| Incoming {
            core_seq: Some(core_seq),
            event: &event,
            at: NOW,
        };
        one.append_from(&hers, &chat, &[at(1)]).await.unwrap();
        let adas = one.leg(&chat, &hers).await.unwrap().unwrap();
        let later = NOW + 2 * CALL_SECS;

        let his = one.call_leg(&chat, &bo, &on_hers, later).await.unwrap();
        assert_eq!(his, Some(adas.clone()));
        one.append_from(&hers, &chat, &[at(2)]).await.unwrap();
        // Ada's call while his holds the turn leaves the leg his.
        let held = one.call_leg(&chat, &ada, &on_hers, later).await.unwrap();
        assert_eq!(held, None);
        let lent = one.leg_on(&chat, &hers).await.unwrap().unwrap();
        assert_eq!((lent.member, lent.number), (bo.clone(), adas.number));
        one.uncall_leg(&chat, &adas, &bo).await.unwrap();
        one.append_from(&hers, &chat, &[at(3)]).await.unwrap();

        assert_eq!(senders(&one).await, ["m1", "m3", "m1"]);
        assert_eq!(called(&one).await, None);
    }
}
