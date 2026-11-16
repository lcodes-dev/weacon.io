//! The machines paired with an account, each with its core's build as it last connected.

use weacon_observability::Fault;
use weacon_protocol::{AccountId, App, Capability, MachineId, MachineSummary};

use super::paired;
use crate::db::{Db, failed};

/// A machine's id, name, system and when it was paired, and its core's build and offers
/// as JSON, as stored.
type Row = (String, String, String, i64, Option<String>, Option<String>);

impl Db {
    /// The machines paired with `account`, oldest first.
    pub(crate) async fn machines_of(
        &self,
        account: &AccountId,
    ) -> Result<Vec<MachineSummary>, Fault> {
        sqlx::query_as::<_, Row>(
            "SELECT machine_id, name, os, paired_at, core_build, core_offers
             FROM machines WHERE account_id = ? ORDER BY paired_at, machine_id",
        )
        .bind(account.as_str())
        .fetch_all(&self.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(stored_machine)
        .collect()
    }

    /// Keeps the build `core` of `machine` and what it `offers`, as it said when it connected.
    pub(crate) async fn saw_core(
        &self,
        machine: &MachineId,
        core: Option<&App>,
        offers: &[Capability],
    ) -> Result<(), Fault> {
        sqlx::query("UPDATE machines SET core_build = ?, core_offers = ? WHERE machine_id = ?")
            .bind(core.and_then(|core| serde_json::to_string(core).ok()))
            .bind(serde_json::to_string(offers).ok())
            .bind(machine.as_str())
            .execute(&self.writer)
            .await
            .map_err(failed)
            .map(drop)
    }
}

fn stored_machine(row: Row) -> Result<MachineSummary, Fault> {
    let (id, name, os, paired_at, core, offers) = row;
    // A value this build can't read shows the machine as not connected yet, not the list as broken.
    let core: Option<App> = core.and_then(|core| serde_json::from_str(&core).ok());
    let offers: Option<Vec<Capability>> =
        offers.and_then(|offers| serde_json::from_str(&offers).ok());
    Ok(MachineSummary {
        core,
        offers: offers.unwrap_or_default(),
        ..paired(id, name, &os, paired_at)?
    })
}

#[cfg(test)]
mod tests {
    use weacon_protocol::code;

    use super::*;
    use crate::db::machine::tests::{Setup, claim, confirm, setup, start};
    use crate::db::test_support::{NOW, id};

    #[tokio::test]
    async fn a_machine_shows_its_cores_build_and_what_it_offers() {
        let Setup { _dir, db, ada, .. } = setup().await;
        start(&db, "p1", NOW).await;
        assert!(confirm(&db, &ada, "p1", "p1").await);
        claim(&db, "p1", "secret", NOW + 1).await;
        let offered = [Capability::new(code!("worktree.open_terminal"))];
        let core = App {
            name: "weacon-core".to_owned(),
            version: "1.2.3".to_owned(),
            commit: "abc".to_owned(),
        };

        let before = db.machines_of(&ada).await.unwrap();
        db.saw_core(&id("mc-p1"), Some(&core), &offered)
            .await
            .unwrap();
        let with_terminal = db.machines_of(&ada).await.unwrap();
        db.saw_core(&id("mc-p1"), None, &[]).await.unwrap();
        let unplain = db.machines_of(&ada).await.unwrap();

        assert_eq!((before[0].core.as_ref(), before[0].offers.len()), (None, 0));
        assert_eq!(with_terminal[0].core.as_ref(), Some(&core));
        assert_eq!(with_terminal[0].offers, offered);
        assert_eq!(unplain[0].core, None);
        assert_eq!(unplain[0].offers, []);
    }

    #[tokio::test]
    async fn a_stored_build_or_offers_this_build_cant_read_show_as_not_connected() {
        let Setup { _dir, db, ada, .. } = setup().await;
        start(&db, "p1", NOW).await;
        assert!(confirm(&db, &ada, "p1", "p1").await);
        claim(&db, "p1", "secret", NOW + 1).await;
        sqlx::query("UPDATE machines SET core_build = 'x', core_offers = 'y'")
            .execute(&db.writer)
            .await
            .unwrap();

        let machines = db.machines_of(&ada).await.unwrap();

        assert_eq!(
            (machines[0].core.as_ref(), machines[0].offers.len()),
            (None, 0)
        );
    }
}
