//! The agents in a chat, one on each machine a member called there: the repository and agent
//! its first prompt fixed, whom it works for, and the last of that machine's events the chat has.

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, ChatId, MachineId, MachineName, MemberId, RepoId, RequestId, RunId, RunState,
};

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

/// A prompt as the server took it from a member.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Asked<'a> {
    pub(crate) member: &'a MemberId,
    pub(crate) request: &'a RequestId,
    pub(crate) text: &'a str,
    /// A machine that is away, as a closed laptop is, whose agent at work holds no one back.
    pub(crate) away: Option<&'a MachineId>,
}

/// How a chat stands: its latest run, its legs, and the machine and member of the call that
/// holds its turn, if one does.
pub(crate) struct Standing {
    pub(crate) runs: weacon_model::ChatRuns,
    pub(crate) legs: Vec<Leg>,
    pub(crate) called: Option<(MachineId, MemberId)>,
}

/// A machine's agent in a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leg {
    /// Whom it works for: the member whose prompt it took last.
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
    /// Calls the leg of `chat` on `new`'s machine with the prompt `asked` at `now`: makes it as
    /// `new` says when the machine has none there, or one that took no prompt and that no call
    /// holds, takes the chat's turn, and notes the prompt. Gives the leg, or none when another
    /// call or leg holds the turn, as [`Self::call`] says. A machine that neither is the
    /// asker's nor is shared with the workspace is `machine.not_found`, and a detached leg
    /// `chat.detached`.
    pub(crate) async fn call_leg(
        &self,
        chat: &ChatId,
        asked: Asked<'_>,
        new: &NewLeg<'_>,
        now: i64,
    ) -> Result<Option<Leg>, Fault> {
        let mut transaction = self.db.write().await?;
        sqlx::query(
            "DELETE FROM chat_legs WHERE workspace_id = ?1 AND chat_id = ?2 AND machine_id = ?3
                 AND core_seq = 0 AND detached = 0
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
        let member = asked.member;
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
        if self
            .busy_in(&mut transaction, chat, called, asked.away)
            .await?
            || !self.call_in(&mut transaction, chat, called, now).await?
        {
            return Ok(None);
        }
        self.asked_in(&mut transaction, chat, new.machine_id, asked)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(Some(leg))
    }

    // Whether another agent of `chat` is at work, so `member`'s call of the leg on `machine`
    // must wait: one working or waiting for an answer, for someone else or on another machine,
    // which still serves the chat.
    async fn busy_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        (machine, member): (&MachineId, &MemberId),
        away: Option<&MachineId>,
    ) -> Result<bool, Fault> {
        let runs = self.read_runs(&mut *connection, chat).await?;
        let legs = self.legs_in(&mut *connection, chat).await?;
        let Some(at_work) = at_work(&runs, &legs) else {
            return Ok(false);
        };
        let theirs = at_work.machine == *machine && at_work.member == *member;
        // An away machine lets others' agents be called, never its own working one.
        let away = Some(&at_work.machine) == away && at_work.machine != *machine;
        if theirs || at_work.detached || away {
            return Ok(false);
        }
        let served = self.leg_on_in(connection, chat, &at_work.machine).await?;
        Ok(served.is_some())
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
        .bind(leg.agent.name())
        .bind(leg.repo.map(RepoId::as_str))
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    // Whether `machine` serves `member`, still of the workspace: it is their own, or shared
    // with the workspace.
    async fn serves_in(
        &self,
        connection: &mut SqliteConnection,
        machine: &MachineId,
        member: &MemberId,
    ) -> Result<bool, Fault> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM members AS m
                 JOIN machines AS d ON d.machine_id = ?3
                 WHERE m.workspace_id = ?1 AND m.member_id = ?2 AND m.account_id IS NOT NULL
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
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        self.legs_in(&mut reader, chat).await
    }

    async fn legs_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
    ) -> Result<Vec<Leg>, Fault> {
        let select = format!(
            "SELECT {} FROM chat_legs AS l WHERE l.workspace_id = ? AND l.chat_id = ?
             ORDER BY l.rowid",
            columns()
        );
        sqlx::query_as::<_, LegRow>(sqlx::AssertSqlSafe(select))
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .fetch_all(connection)
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
            "UPDATE chat_legs SET quoted_seq = MAX(quoted_seq, ?)
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
            "UPDATE chats SET called_by = ?1, called_on = ?2, called_at = ?3, called_for = NULL
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

    /// Takes back the call `asked` made of the leg of `chat` on `machine`, which never reached
    /// it: the turn is free, and a prompt the machine sends up under that request isn't theirs.
    pub(crate) async fn uncall_leg(
        &self,
        chat: &ChatId,
        machine: &MachineId,
        asked: Asked<'_>,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        // A later call of theirs that holds the turn by now keeps it.
        sqlx::query(
            "UPDATE chats
             SET called_by = NULL, called_on = NULL, called_at = NULL, called_for = NULL
             WHERE workspace_id = ? AND chat_id = ? AND called_for = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(asked.request.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.take_in(&mut transaction, chat, machine, asked.request)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Notes the prompt `asked` to `chat`, made of the leg on `machine`, as the server took it;
    /// `chat.request_reused` while its request is on record for a prompt already.
    pub(crate) async fn asked(
        &self,
        chat: &ChatId,
        machine: &MachineId,
        asked: Asked<'_>,
    ) -> Result<(), Fault> {
        let mut writer = self.db.writer.acquire().await.map_err(failed)?;
        self.asked_in(&mut writer, chat, machine, asked).await
    }

    async fn asked_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
        asked: Asked<'_>,
    ) -> Result<(), Fault> {
        // A request stands for one prompt until its machine sends it up or it is taken back.
        let noted = sqlx::query(
            "INSERT INTO chat_calls
                 (workspace_id, chat_id, request_id, machine_id, member_id, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT DO NOTHING",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(asked.request.as_str())
        .bind(machine.as_str())
        .bind(asked.member.as_str())
        .bind(asked.text)
        .execute(&mut *connection)
        .await
        .map_err(failed)?;
        if noted.rows_affected() == 0 {
            return Err(Fault::new(
                weacon_protocol::code!("chat.request_reused"),
                "That request is another prompt's",
            ));
        }
        // The turn its call just took is this request's to give back.
        sqlx::query(
            "UPDATE chats SET called_for = ?
             WHERE workspace_id = ? AND chat_id = ? AND called_by = ? AND called_on = ?",
        )
        .bind(asked.request.as_str())
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(asked.member.as_str())
        .bind(machine.as_str())
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Who made the prompt `request` of the leg of `chat` on `machine`, and its words, while
    /// the server holds it: taken from a member, and not yet stored or taken back.
    pub(crate) async fn asker(
        &self,
        chat: &ChatId,
        machine: &MachineId,
        request: &RequestId,
    ) -> Result<Option<(MemberId, String)>, Fault> {
        let held = sqlx::query_as::<_, (String, String)>(
            "SELECT member_id, text FROM chat_calls
             WHERE workspace_id = ? AND chat_id = ? AND request_id = ? AND machine_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(request.as_str())
        .bind(machine.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?;
        let Some((member, text)) = held else {
            return Ok(None);
        };
        Ok(Some((stored(member)?, text)))
    }

    // Uses up the record of the prompt `request` made of the leg of `chat` on `machine`, and
    // says who made it.
    pub(super) async fn take_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
        request: &RequestId,
    ) -> Result<Option<MemberId>, Fault> {
        sqlx::query_scalar::<_, String>(
            "DELETE FROM chat_calls
             WHERE workspace_id = ? AND chat_id = ? AND request_id = ? AND machine_id = ?
             RETURNING member_id",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(request.as_str())
        .bind(machine.as_str())
        .fetch_optional(connection)
        .await
        .map_err(failed)?
        .map(stored)
        .transpose()
    }

    // Has the leg of `chat` on `machine` work for `member`, whose prompt it took, while the
    // machine serves them.
    pub(super) async fn lend_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        machine: &MachineId,
        member: &MemberId,
    ) -> Result<(), Fault> {
        if !self.serves_in(&mut *connection, machine, member).await? {
            return Ok(());
        }
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

    /// `chat`'s run, legs and the call that holds its turn at `now`, read in one transaction.
    pub(crate) async fn standing(&self, chat: &ChatId, now: i64) -> Result<Standing, Fault> {
        let mut reader = self.db.readers.begin().await.map_err(failed)?;
        let runs = self.read_runs(&mut reader, chat).await?;
        let legs = self.legs_in(&mut reader, chat).await?;
        let held = sqlx::query_as::<_, (String, String)>(
            "SELECT called_on, called_by FROM chats
             WHERE workspace_id = ? AND chat_id = ? AND called_on IS NOT NULL
                 AND called_at > ? - ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(now)
        .bind(CALL_SECS)
        .fetch_optional(&mut *reader)
        .await
        .map_err(failed)?;
        let called = match held {
            Some((machine, member)) => Some((stored(machine)?, stored(member)?)),
            None => None,
        };
        Ok(Standing { runs, legs, called })
    }

    /// [`Self::uncall_in`], on a connection of its own.
    #[cfg(test)]
    pub(crate) async fn uncall(
        &self,
        chat: &ChatId,
        called: (&MachineId, &MemberId),
    ) -> Result<bool, Fault> {
        let mut writer = self.db.writer.acquire().await.map_err(failed)?;
        self.uncall_in(&mut writer, chat, called).await
    }

    /// Gives back the turn `member`'s call of the leg on `machine` took in `chat`, as once its
    /// run reports or when it never reached its machine; whether that call held the turn.
    pub(super) async fn uncall_in(
        &self,
        connection: &mut SqliteConnection,
        chat: &ChatId,
        (machine, member): (&MachineId, &MemberId),
    ) -> Result<bool, Fault> {
        let given = sqlx::query(
            "UPDATE chats
             SET called_by = NULL, called_on = NULL, called_at = NULL, called_for = NULL
             WHERE workspace_id = ? AND chat_id = ? AND called_by = ? AND called_on = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .bind(machine.as_str())
        .execute(connection)
        .await
        .map_err(failed)?;
        Ok(given.rows_affected() > 0)
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

/// Which of `legs` works or waits for an answer, as the chat's `runs` say, if one does.
pub(crate) fn at_work<'a>(runs: &weacon_model::ChatRuns, legs: &'a [Leg]) -> Option<&'a Leg> {
    let working = |state| {
        matches!(
            state,
            RunState::Starting | RunState::Running | RunState::Waiting
        )
    };
    let run = runs.run().filter(|run| working(run.state))?;
    let named = legs.iter().find(|leg| leg.own_run(&run.run).is_some());
    // A run from before legs named theirs is the chat's first leg's.
    named.or_else(|| legs.first())
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
        .find(|kind| kind.name() == name)
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

    static R1: std::sync::LazyLock<RequestId> = std::sync::LazyLock::new(|| id("r-1"));
    static R2: std::sync::LazyLock<RequestId> = std::sync::LazyLock::new(|| id("r-2"));

    use weacon_protocol::ChatEvent;

    use super::*;
    use crate::db::Incoming;
    use crate::db::test_support::{NOW, WORDS, ask, code, id, make, said, status};
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

        let first = one
            .call_leg(&chat, ask(&bo, &R1), &on(&shelf, codex), NOW)
            .await;
        let held = one
            .call_leg(&chat, ask(&bo, &R2), &on(&desk, codex), NOW + 1)
            .await;

        assert_eq!(first.unwrap().unwrap().machine.as_str(), "mc-4");
        assert_eq!(held.unwrap(), None);
        assert_eq!(one.leg(&chat, &desk).await.unwrap(), None);
        // No one else's starts meanwhile either, but his next call of the same leg goes.
        let adas = (&id::<MachineId>("mc-1"), &member_id("m1"));
        assert!(!one.call(&chat, adas, NOW + 1).await.unwrap());
        let r3 = id("r-3");
        let again = one
            .call_leg(&chat, ask(&bo, &r3), &on(&shelf, codex), NOW + 1)
            .await;
        assert!(again.unwrap().is_some());
        // Once the hold has run out, another leg is called, and one that took no prompt is
        // made again as its next call says.
        let later = NOW + 1 + CALL_SECS;
        let other = one
            .call_leg(&chat, ask(&bo, &R2), &on(&desk, codex), later)
            .await;
        assert!(other.unwrap().is_some());
        let claude = on(&shelf, AgentKind::Claude);
        let r4 = id("r-4");
        let remade = one
            .call_leg(&chat, ask(&bo, &r4), &claude, later + CALL_SECS)
            .await;
        assert_eq!(remade.unwrap().unwrap().agent, AgentKind::Claude);
        // One that was detached before it took a prompt stays so, and is never made again.
        let never = later + 3 * CALL_SECS;
        one.uncall(&chat, (&shelf, &bo)).await.unwrap();
        let gone = one.leg(&chat, &shelf).await.unwrap().unwrap();
        one.detach_leg(&chat, &gone).await.unwrap();
        let ended = one
            .call_leg(&chat, ask(&bo, &R1), &on(&shelf, codex), never)
            .await;
        assert_eq!(code(&ended.unwrap_err()), "chat.detached");
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
        let called = one.call_leg(&chat, ask(&bo, &R1), &codex, NOW).await;
        called.unwrap().unwrap();
        one.uncall(&chat, (&desk, &bo)).await.unwrap();
        let stolen = one.call_leg(&chat, ask(&bo, &R1), &stray, NOW).await;

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
        let stored = one.append_from((&desk, &bo), &chat, &[first]).await;
        assert_eq!(stored.unwrap(), 1);
        let claude = on(&desk, AgentKind::Claude);
        let again = one.call_leg(&chat, ask(&bo, &R2), &claude, NOW).await;
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

        let unshared = one.call_leg(&chat, ask(&ada, &R1), &on_desk, at(1)).await;
        db.set_shares(&desk, &w1).await.unwrap();
        one.call_leg(&chat, ask(&ada, &R1), &on_desk, at(1))
            .await
            .unwrap();
        let event = status(RunState::Idle);
        let first = Incoming {
            core_seq: Some(1),
            event: &event,
            at: NOW,
        };
        one.append_from((&desk, &ada), &chat, &[first])
            .await
            .unwrap();
        let hers = one.leg(&chat, &desk).await.unwrap().unwrap();
        // His call of the same leg waits for hers, as anyone's would.
        let held = one.call_leg(&chat, ask(&bo, &R2), &on_desk, at(1)).await;
        let his = one.call_leg(&chat, ask(&bo, &R2), &on_desk, at(2)).await;
        // A request on record as hers is never made his.
        let reused = one.call_leg(&chat, ask(&bo, &R1), &on_desk, at(2)).await;
        assert_eq!(code(&reused.unwrap_err()), "chat.request_reused");
        db.set_shares(&desk, &[]).await.unwrap();
        let ended = one.call_leg(&chat, ask(&ada, &R1), &on_desk, at(3)).await;
        let r3 = id("r-3");
        let owners = one.call_leg(&chat, ask(&bo, &r3), &on_desk, at(3)).await;

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
    async fn a_call_is_refused_while_another_agent_is_at_work_though_the_turn_is_free() {
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
        // Her run's first word gave back the turn her call held, as it does.
        let running = status(RunState::Running);
        let reported = [Incoming {
            core_seq: Some(1),
            event: &running,
            at: NOW,
        }];
        one.append_from((&hers, &ada), &chat, &reported)
            .await
            .unwrap();
        assert_eq!(called(&one).await, None);

        let his = one.call_leg(&chat, ask(&bo, &R1), &on_hers, NOW + 1).await;

        assert_eq!(his.unwrap(), None);
        assert_eq!(called(&one).await, None);
        assert_eq!(one.asker(&chat, &hers, &R1).await.unwrap(), None);
        // Her own next prompt goes.
        let more = one.call_leg(&chat, ask(&ada, &R2), &on_hers, NOW + 1).await;
        assert!(more.unwrap().is_some());
        one.uncall_leg(&chat, &hers, ask(&ada, &R2)).await.unwrap();
        // While her machine is away his own agent is called, never hers that works for her.
        one.pair_for("m3", "mc-3").await;
        let (his, desk) = (id::<MachineId>("mc-3"), id("Bo's desk"));
        let on_his = NewLeg {
            machine_id: &his,
            machine: &desk,
            ..on_hers
        };
        let past = Asked {
            away: Some(&hers),
            ..ask(&bo, &R1)
        };
        let meanwhile = one.call_leg(&chat, ask(&bo, &R1), &on_his, NOW + 1).await;
        assert_eq!(meanwhile.unwrap(), None);
        let onto_hers = one.call_leg(&chat, past, &on_hers, NOW + 1).await;
        assert_eq!(onto_hers.unwrap(), None);
        let own = one.call_leg(&chat, past, &on_his, NOW + 1).await;
        assert!(own.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_member_who_left_the_workspace_calls_a_machine_shared_with_it_no_more() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m1", "mc-1").await;
        one.db.set_shares(&id("mc-1"), &[id("w1")]).await.unwrap();
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (hers, name) = (id::<MachineId>("mc-1"), id("Ada's laptop"));
        let on_hers = NewLeg {
            machine_id: &hers,
            machine: &name,
            agent: AgentKind::Claude,
            repo: None,
        };

        let before = one.call_leg(&chat, ask(&bo, &R1), &on_hers, NOW).await;
        assert!(before.unwrap().is_some());
        one.uncall_leg(&chat, &hers, ask(&bo, &R1)).await.unwrap();

        one.leave(&bo).await.unwrap();

        let left = one.call_leg(&chat, ask(&bo, &R1), &on_hers, NOW).await;
        assert_eq!(code(&left.unwrap_err()), "machine.not_found");
    }

    #[tokio::test]
    async fn a_request_stands_for_one_prompt_until_it_is_stored_or_taken_back() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        one.pair_for("m3", "mc-3").await;
        let (chat, bo) = (id::<ChatId>("c-1"), member_id("m3"));
        let (name, desk) = (id("Bo's desk"), id::<MachineId>("mc-3"));
        let on_desk = NewLeg {
            machine_id: &desk,
            machine: &name,
            agent: AgentKind::Codex,
            repo: None,
        };
        let other = Asked {
            text: "Other words",
            ..ask(&bo, &R1)
        };
        one.call_leg(&chat, ask(&bo, &R1), &on_desk, NOW)
            .await
            .unwrap();

        // Made again, even by him and in other words, it is refused, and nothing changes.
        let again = one.call_leg(&chat, other, &on_desk, NOW + 1).await;

        assert_eq!(code(&again.unwrap_err()), "chat.request_reused");
        assert_eq!(called(&one).await, Some(("m3".to_owned(), NOW)));
        let noted = one.asker(&chat, &desk, &R1).await.unwrap();
        assert_eq!(noted, Some((bo.clone(), WORDS.to_owned())));
        // Taken back, the request is free again.
        one.uncall_leg(&chat, &desk, ask(&bo, &R1)).await.unwrap();
        let free = one.call_leg(&chat, other, &on_desk, NOW + 2).await;
        assert!(free.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_call_taken_back_frees_its_turn_and_forgets_its_prompt() {
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
        one.db.set_shares(&desk, &[id("w1")]).await.unwrap();
        let (hers, his) = (ask(&ada, &R1), ask(&bo, &R2));
        one.call_leg(&chat, hers, &on_desk, NOW).await.unwrap();
        let noted = one.asker(&chat, &desk, &R1).await.unwrap();
        assert_eq!(noted, Some((ada.clone(), WORDS.to_owned())));

        one.uncall_leg(&chat, &desk, hers).await.unwrap();

        assert_eq!(one.asker(&chat, &desk, &R1).await.unwrap(), None);
        assert_eq!(called(&one).await, None);
        // His, taken back, leaves the turn her later call holds.
        let later = NOW + 2 * CALL_SECS;
        one.call_leg(&chat, his, &on_desk, later).await.unwrap();
        one.call_leg(&chat, hers, &on_desk, later + CALL_SECS)
            .await
            .unwrap();
        one.uncall_leg(&chat, &desk, his).await.unwrap();
        let held = called(&one).await;
        assert_eq!(held, Some(("m1".to_owned(), later + CALL_SECS)));
        // So does an earlier call of her own, taken back once a later one holds the turn.
        let (r3, last) = (id("r-3"), later + CALL_SECS + 1);
        let again = ask(&ada, &r3);
        one.call_leg(&chat, again, &on_desk, last).await.unwrap();
        one.uncall_leg(&chat, &desk, hers).await.unwrap();
        assert_eq!(called(&one).await, Some(("m1".to_owned(), last)));
        assert!(one.asker(&chat, &desk, &r3).await.unwrap().is_some());
        one.uncall_leg(&chat, &desk, again).await.unwrap();
        assert_eq!(called(&one).await, None);
        // What an agent was given as quotes never moves back.
        one.quoted(&chat, &desk, 5).await.unwrap();
        one.quoted(&chat, &desk, 3).await.unwrap();
        let left = one.leg(&chat, &desk).await.unwrap().unwrap();
        assert_eq!(left.quoted_seq, 5);
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
        // Only the call that holds the turn gives it back: not his, nor hers of another leg.
        assert!(!one.uncall(&chat, bos).await.unwrap());
        assert!(!one.uncall(&chat, (&his, &ada)).await.unwrap());
        assert!(!one.call(&chat, bos, NOW + 2).await.unwrap());
        assert!(one.call(&chat, bos, NOW + 1 + CALL_SECS).await.unwrap());
        assert!(one.uncall(&chat, bos).await.unwrap());
        let last = NOW + 3 + CALL_SECS;
        assert!(one.call(&chat, adas, last).await.unwrap());
        // It says which leg she means only while it holds the turn.
        let meant = one.standing(&chat, last + CALL_SECS - 1).await;
        assert_eq!(meant.unwrap().called, Some((hers.clone(), ada.clone())));
        let stale = one.standing(&chat, last + CALL_SECS).await;
        assert_eq!(stale.unwrap().called, None);
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
        let (chat, ada, bo) = (id::<ChatId>("c-1"), member_id("m1"), member_id("m3"));
        let (name, hers, his) = (id("Bo's desk"), id("mc-1"), id("mc-3"));
        let leg = NewLeg {
            machine_id: &his,
            machine: &name,
            agent: AgentKind::Claude,
            repo: None,
        };
        one.call_leg(&chat, ask(&bo, &R1), &leg, NOW)
            .await
            .unwrap()
            .unwrap();
        let event = status(RunState::Idle);
        let at = |core_seq| Incoming {
            core_seq: Some(core_seq),
            event: &event,
            at: NOW,
        };

        let stored = [
            one.append_from((&hers, &ada), &chat, &[at(1), at(2)]).await,
            one.append_from((&his, &bo), &chat, &[at(1)]).await,
            // A replay from one machine is skipped, whatever the other sent since.
            one.append_from((&hers, &ada), &chat, &[at(2), at(3)]).await,
            one.append_from((&his, &bo), &chat, &[at(1), at(2)]).await,
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
    async fn a_leg_works_for_the_member_whose_prompt_it_took_last() {
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
        let (idle, prompt) = (status(RunState::Idle), prompted("r-1"));
        let at = |core_seq, event| Incoming {
            core_seq: Some(core_seq),
            event,
            at: NOW,
        };
        one.append_from((&hers, &ada), &chat, &[at(1, &idle)])
            .await
            .unwrap();
        let adas = one.leg(&chat, &hers).await.unwrap().unwrap();
        let later = NOW + 2 * CALL_SECS;

        // His call alone leaves it hers: its machine has yet to take his prompt.
        let his = one.call_leg(&chat, ask(&bo, &R1), &on_hers, later).await;
        assert_eq!(his.unwrap(), Some(adas.clone()));
        let taken = (Some(&*R1), at(2, &prompt));
        one.append_prompt((&hers, &bo), taken.0, &chat, taken.1)
            .await
            .unwrap();

        let lent = one.leg_on(&chat, &hers).await.unwrap().unwrap();
        assert_eq!((&lent.member, lent.number), (&bo, adas.number));
        // His prompt's record is used up, and what the machine sends as hers is refused.
        let again = (Some(&*R1), at(3, &prompt));
        let again = one.append_prompt((&hers, &bo), again.0, &chat, again.1);
        assert_eq!(code(&again.await.unwrap_err()), "chat.leg_called");
        let stale = [at(3, &idle)];
        let stale = one.append_from((&hers, &ada), &chat, &stale).await;
        assert_eq!(code(&stale.unwrap_err()), "chat.leg_called");
        // A prompt the chat already has, sent again, moves nothing.
        one.append_prompt((&hers, &ada), None, &chat, at(2, &prompt))
            .await
            .unwrap();
        let kept = one.leg_on(&chat, &hers).await.unwrap().unwrap();
        assert_eq!(kept.member, bo);
        // One written on the machine is its owner's, and the leg hers again.
        one.append_prompt((&hers, &ada), None, &chat, at(3, &prompt))
            .await
            .unwrap();
        let back = one.leg_on(&chat, &hers).await.unwrap().unwrap();
        assert_eq!(back.member, ada);
        assert_eq!(senders(&one).await, ["m1", "m3", "m1"]);
    }

    fn prompted(request: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: id(request),
            text: WORDS.to_owned(),
            author: None,
            pick: None,
            repo: None,
            agent: None,
            task: None,
        }
    }
}
