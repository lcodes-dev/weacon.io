//! The workspaces a machine is shared with: every member of one may call agents on it, for as
//! long as its owner is a member too and keeps it paired.

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{AccountId, MachineId, MachineName, SharedMachine, WorkspaceId};

use crate::db::changes::Versioned;
use crate::db::{Db, Workspace, failed, stored};

/// The member of leg `l`'s workspace who owns its machine and shares it with that workspace;
/// null for a machine that isn't shared with it.
pub(in crate::db) const SHARER: &str = "(SELECT o.member_id FROM shared_machines AS s
    JOIN machines AS sd ON sd.machine_id = s.machine_id
    JOIN members AS o ON o.workspace_id = s.workspace_id AND o.account_id = sd.account_id
    WHERE s.workspace_id = l.workspace_id AND s.machine_id = l.machine_id)";

impl Db {
    /// Keeps `workspaces`, those of them its owner is a member of, as all that `machine` is
    /// shared with; gives the workspaces it is newly shared with or no longer, whose feeds
    /// hear so. Where it is shared no longer, its agents work for its owner from then on.
    pub(crate) async fn set_shares(
        &self,
        machine: &MachineId,
        workspaces: &[WorkspaceId],
    ) -> Result<Vec<WorkspaceId>, Fault> {
        let mut transaction = self.write().await?;
        let before = shares_in(&mut transaction, machine).await?;
        sqlx::query("DELETE FROM shared_machines WHERE machine_id = ?")
            .bind(machine.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        for workspace in workspaces {
            sqlx::query(
                "INSERT OR IGNORE INTO shared_machines (workspace_id, machine_id)
                 SELECT m.workspace_id, d.machine_id FROM machines AS d
                 JOIN members AS m ON m.account_id = d.account_id
                 WHERE d.machine_id = ? AND m.workspace_id = ?",
            )
            .bind(machine.as_str())
            .bind(workspace.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        let after = shares_in(&mut transaction, machine).await?;
        let both = before.iter().chain(&after);
        let changed = both.filter(|one| before.contains(one) != after.contains(one));
        let changed: Vec<WorkspaceId> = changed.cloned().collect();
        for workspace in &changed {
            let ended = !after.contains(workspace);
            self.changed_in(&mut transaction, (workspace, machine), ended)
                .await?;
        }
        transaction.commit().await.map_err(failed)?;
        Ok(changed)
    }

    // Records for `workspace`'s feeds that `machine`'s sharing with it changed, as its owner's
    // doing while they are a member there; once it `ended`, the machine's legs there are its
    // owner's alone.
    async fn changed_in(
        &self,
        transaction: &mut SqliteConnection,
        (workspace, machine): (&WorkspaceId, &MachineId),
        ended: bool,
    ) -> Result<(), Fault> {
        let owner = sqlx::query_scalar::<_, String>(
            "SELECT m.member_id FROM members AS m
             JOIN machines AS d ON d.account_id = m.account_id
             WHERE m.workspace_id = ? AND d.machine_id = ?",
        )
        .bind(workspace.as_str())
        .bind(machine.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let Some(owner) = owner else {
            return Ok(());
        };
        if ended {
            sqlx::query(
                "UPDATE chat_legs SET member_id = ? WHERE workspace_id = ? AND machine_id = ?",
            )
            .bind(&owner)
            .bind(workspace.as_str())
            .bind(machine.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        let place = Workspace {
            db: self.clone(),
            id: workspace.clone(),
        };
        place
            .record_change(transaction, Versioned::Machines, &stored(owner)?)
            .await
    }
}

// The workspaces `machine` is shared with, in order.
async fn shares_in(
    connection: &mut SqliteConnection,
    machine: &MachineId,
) -> Result<Vec<WorkspaceId>, Fault> {
    sqlx::query_scalar::<_, String>(
        "SELECT workspace_id FROM shared_machines WHERE machine_id = ? ORDER BY workspace_id",
    )
    .bind(machine.as_str())
    .fetch_all(connection)
    .await
    .map_err(failed)?
    .into_iter()
    .map(stored)
    .collect()
}

impl Workspace {
    /// The machines shared with this workspace, each with its owner's member here, by name.
    /// None is shown as connected.
    pub(crate) async fn shared_machines(&self) -> Result<Vec<SharedMachine>, Fault> {
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT d.machine_id, d.name, o.member_id FROM shared_machines AS s
             JOIN machines AS d ON d.machine_id = s.machine_id
             JOIN members AS o ON o.workspace_id = s.workspace_id AND o.account_id = d.account_id
             WHERE s.workspace_id = ? ORDER BY d.name, d.machine_id",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, name, owner)| {
            Ok(SharedMachine {
                id: stored(id)?,
                name: stored(name)?,
                owner: stored(owner)?,
                connected: false,
            })
        })
        .collect()
    }

    /// What `machine` is called, if `account` may call agents on it in this workspace: its
    /// own, or one shared with the workspace.
    pub(crate) async fn callable(
        &self,
        account: &AccountId,
        machine: &MachineId,
    ) -> Result<Option<MachineName>, Fault> {
        sqlx::query_scalar::<_, String>(
            "SELECT d.name FROM machines AS d
             WHERE d.machine_id = ?1 AND (d.account_id = ?2 OR EXISTS (
                 SELECT 1 FROM shared_machines AS s
                 JOIN members AS o ON o.workspace_id = s.workspace_id
                     AND o.account_id = d.account_id
                 WHERE s.workspace_id = ?3 AND s.machine_id = d.machine_id))",
        )
        .bind(machine.as_str())
        .bind(account.as_str())
        .bind(self.id.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(stored)
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    static R1: std::sync::LazyLock<weacon_protocol::RequestId> =
        std::sync::LazyLock::new(|| id("r-1"));

    use super::*;
    use crate::db::NewLeg;
    use crate::db::test_support::{NOW, ask, id, make};
    use crate::test_support::{member_id, two_teams};

    async fn account_of(workspace: &Workspace, member: &str) -> AccountId {
        let found = sqlx::query_scalar::<_, String>(
            "SELECT account_id FROM members WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(workspace.id.as_str())
        .bind(member)
        .fetch_one(&workspace.db.readers)
        .await;
        id(&found.unwrap())
    }

    fn names(machines: &[SharedMachine]) -> Vec<(&str, &str)> {
        let mut shown = Vec::new();
        for machine in machines {
            shown.push((machine.id.as_str(), machine.owner.as_str()));
        }
        shown
    }

    #[tokio::test]
    async fn a_machine_is_shared_only_with_workspaces_its_owner_is_in_and_only_as_last_said() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        one.pair_for("m1", "mc-1").await;
        one.pair_for("m3", "mc-3").await;
        let (db, adas, bos) = (&one.db, id::<MachineId>("mc-1"), id::<MachineId>("mc-3"));
        let (ada, bo) = (account_of(&one, "m1").await, account_of(&one, "m3").await);
        let both = [id("w1"), id("w2"), id("w1")];

        // Bo is in w1 only, so his core naming w2 shares nothing there.
        db.set_shares(&bos, &both).await.unwrap();
        assert_eq!(
            names(&one.shared_machines().await.unwrap()),
            [("mc-3", "m3")]
        );
        assert!(two.shared_machines().await.unwrap().is_empty());
        assert!(one.callable(&ada, &bos).await.unwrap().is_some());
        assert_eq!(two.callable(&ada, &bos).await.unwrap(), None);
        // Each machine is its owner's own to call wherever they are, shared or not.
        assert!(two.callable(&ada, &adas).await.unwrap().is_some());
        assert_eq!(one.callable(&bo, &adas).await.unwrap(), None);
        assert_eq!(one.callable(&bo, &id("mc-none")).await.unwrap(), None);
        db.set_shares(&adas, &both[1..2]).await.unwrap();
        assert_eq!(
            names(&two.shared_machines().await.unwrap()),
            [("mc-1", "m2")]
        );
        assert_eq!(
            names(&one.shared_machines().await.unwrap()),
            [("mc-3", "m3")]
        );
        db.set_shares(&bos, &[]).await.unwrap();
        assert!(one.shared_machines().await.unwrap().is_empty());
        assert_eq!(one.callable(&ada, &bos).await.unwrap(), None);
    }

    async fn detached(workspace: &Workspace, chat: &str, machine: &MachineId) -> bool {
        let leg = workspace.leg(&id(chat), machine).await.unwrap().unwrap();
        leg.detached
    }

    #[tokio::test]
    async fn a_share_starting_or_ending_is_news_to_its_workspace_and_ends_no_agent() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        one.pair_for("m3", "mc-3").await;
        let (db, desk) = (&one.db, id::<MachineId>("mc-3"));
        let (ada, name) = (member_id("m1"), id("Bo's desk"));
        let on_desk = NewLeg {
            machine_id: &desk,
            machine: &name,
            agent: weacon_protocol::AgentKind::Claude,
            repo: None,
        };
        make(&one, "c-1", "m1", NOW).await.unwrap();
        let chat = id("c-1");

        let begun = db.set_shares(&desk, &[id("w1"), id("w-none")]).await;
        let called = one
            .call_leg(&chat, ask(&ada, &R1), &on_desk, NOW + 1_000)
            .await;
        called.unwrap().unwrap();
        // Saying the same again changes nothing.
        let same = db.set_shares(&desk, &[id("w1")]).await.unwrap();
        let before = one.changes_since(0, 99).await.unwrap().len();
        let ended = db.set_shares(&desk, &[]).await.unwrap();

        // Its feeds hear the share end, and nothing of the chat.
        let told = one.changes_since(0, 99).await.unwrap();
        let news: Vec<_> = told.iter().skip(before).map(|one| &one.change).collect();
        assert_eq!(news, [&weacon_protocol::WorkspaceChange::MachinesChanged]);
        // What she called there still reports, as its owner's agent from here on.
        assert!(!detached(&one, "c-1", &desk).await);
        let left = one.leg_on(&chat, &desk).await.unwrap().unwrap();
        assert_eq!((left.member, left.shared_by), (member_id("m3"), None));
        let all = (begun.unwrap(), same, ended);
        assert_eq!(all, (vec![id("w1")], vec![], vec![id("w1")]));
    }

    #[tokio::test]
    async fn unpairing_a_machine_or_its_owner_leaving_ends_its_sharing_at_once() {
        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        one.pair_for("m3", "mc-3").await;
        one.pair_for("m3", "mc-4").await;
        let (db, desk, shelf) = (&one.db, id::<MachineId>("mc-3"), id::<MachineId>("mc-4"));
        let (ada, bo) = (account_of(&one, "m1").await, account_of(&one, "m3").await);
        db.set_shares(&desk, &[id("w1")]).await.unwrap();
        db.set_shares(&shelf, &[id("w1")]).await.unwrap();
        assert_eq!(one.shared_machines().await.unwrap().len(), 2);

        assert!(db.unpair(&bo, &desk).await.unwrap());

        assert_eq!(
            names(&one.shared_machines().await.unwrap()),
            [("mc-4", "m3")]
        );
        assert_eq!(one.callable(&ada, &desk).await.unwrap(), None);
        // What Ada ran on Bo's other machine goes with him, for good.
        make(&one, "c-1", "m1", NOW).await.unwrap();
        let name = id("Bo's shelf");
        let on_shelf = NewLeg {
            machine_id: &shelf,
            machine: &name,
            agent: weacon_protocol::AgentKind::Claude,
            repo: None,
        };
        let (chat, adas) = (id("c-1"), member_id("m1"));
        let called = one
            .call_leg(&chat, ask(&adas, &R1), &on_shelf, NOW + 1_000)
            .await;
        called.unwrap().unwrap();
        assert!(!detached(&one, "c-1", &shelf).await);
        one.leave(&member_id("m3")).await.unwrap();
        assert!(detached(&one, "c-1", &shelf).await);
        assert!(one.shared_machines().await.unwrap().is_empty());
        assert_eq!(one.callable(&ada, &shelf).await.unwrap(), None);
    }
}
