//! The workspaces a machine is shared with: every member of one may call agents on it, for as
//! long as its owner is a member too and keeps it paired.

use weacon_observability::Fault;
use weacon_protocol::{AccountId, MachineId, MachineName, SharedMachine, WorkspaceId};

use crate::db::{Db, Workspace, failed, stored};

/// The member of leg `l`'s workspace who owns its machine and shares it with that workspace;
/// null for a machine that isn't shared with it.
pub(in crate::db) const SHARER: &str = "(SELECT o.member_id FROM shared_machines AS s
    JOIN machines AS sd ON sd.machine_id = s.machine_id
    JOIN members AS o ON o.workspace_id = s.workspace_id AND o.account_id = sd.account_id
    WHERE s.workspace_id = l.workspace_id AND s.machine_id = l.machine_id)";

impl Db {
    /// Keeps `workspaces`, those of them its owner is a member of, as all that `machine` is
    /// shared with.
    pub(crate) async fn set_shares(
        &self,
        machine: &MachineId,
        workspaces: &[WorkspaceId],
    ) -> Result<(), Fault> {
        let mut transaction = self.write().await?;
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
        // What other members ran on it where it is no longer shared can't be called again, even
        // if it is shared there once more.
        sqlx::query(
            "UPDATE chat_legs SET detached = 1
             WHERE machine_id = ?1
                 AND NOT EXISTS (SELECT 1 FROM shared_machines AS s
                     WHERE s.workspace_id = chat_legs.workspace_id AND s.machine_id = ?1)
                 AND NOT EXISTS (SELECT 1 FROM members AS m
                     JOIN machines AS d ON d.account_id = m.account_id
                     WHERE m.workspace_id = chat_legs.workspace_id
                         AND m.member_id = chat_legs.member_id AND d.machine_id = ?1)",
        )
        .bind(machine.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }
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

    use super::*;
    use crate::db::test_support::id;
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

    async fn detached(
        workspace: &Workspace,
        chat: &str,
        member: &weacon_protocol::MemberId,
    ) -> bool {
        let leg = workspace.leg(&id(chat), member).await.unwrap().unwrap();
        leg.detached
    }

    #[tokio::test]
    async fn ending_a_share_detaches_the_other_members_agents_for_good_and_never_the_owners() {
        use crate::db::NewLeg;
        use crate::db::test_support::{NOW, make};

        let dir = TempDir::new().unwrap();
        let (one, _two) = two_teams(&dir).await;
        one.pair_for("m3", "mc-3").await;
        let (db, desk) = (&one.db, id::<MachineId>("mc-3"));
        let (ada, bo) = (member_id("m1"), member_id("m3"));
        let name = id("Bo's desk");
        let on_desk = NewLeg {
            machine_id: &desk,
            machine: &name,
            agent: weacon_protocol::AgentKind::Claude,
            repo: None,
        };
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&one, "c-2", "m3", NOW).await.unwrap();
        db.set_shares(&desk, &[id("w1")]).await.unwrap();
        let later = NOW + 1_000;
        one.set_leg(&id("c-1"), &ada, &on_desk, later)
            .await
            .unwrap();
        one.set_leg(&id("c-2"), &bo, &on_desk, later).await.unwrap();
        // Saying the same again changes nothing.
        db.set_shares(&desk, &[id("w1")]).await.unwrap();
        let kept = detached(&one, "c-1", &ada).await;
        db.set_shares(&desk, &[]).await.unwrap();
        db.set_shares(&desk, &[id("w1")]).await.unwrap();

        assert!(!kept);
        assert!(detached(&one, "c-1", &ada).await);
        assert!(!detached(&one, "c-2", &bo).await);
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
        one.leave(&member_id("m3")).await.unwrap();
        assert!(one.shared_machines().await.unwrap().is_empty());
        assert_eq!(one.callable(&ada, &shelf).await.unwrap(), None);
    }
}
