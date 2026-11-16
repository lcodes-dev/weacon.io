//! The machines paired with an account, each with its core's build as it last connected.

use weacon_observability::Fault;
use weacon_protocol::{AccountId, App, Capability, MachineId, MachineSummary};

use super::paired;
use crate::db::{Db, failed};

/// A machine's id, name, system, key and when it was paired, and its core's build and offers
/// as JSON, as stored.
type Row = (
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
);

impl Db {
    /// The machines paired with `account`, oldest first, each with the `offered` capabilities
    /// its core didn't offer back when it last connected.
    pub(crate) async fn machines_of(
        &self,
        account: &AccountId,
        offered: &[Capability],
    ) -> Result<Vec<MachineSummary>, Fault> {
        sqlx::query_as::<_, Row>(
            "SELECT machine_id, name, os, machine_key, paired_at, core_build, core_offers
             FROM machines WHERE account_id = ? ORDER BY paired_at, machine_id",
        )
        .bind(account.as_str())
        .fetch_all(&self.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|row| stored_machine(row, offered))
        .collect()
    }

    /// Keeps the build `core` of `machine` and what it `offers`, as it said when it connected;
    /// with no offers kept, it is never shown lacking any.
    pub(crate) async fn saw_core(
        &self,
        machine: &MachineId,
        core: Option<&App>,
        offers: Option<&[Capability]>,
    ) -> Result<(), Fault> {
        sqlx::query("UPDATE machines SET core_build = ?, core_offers = ? WHERE machine_id = ?")
            .bind(core.and_then(|core| serde_json::to_string(core).ok()))
            .bind(offers.and_then(|offers| serde_json::to_string(offers).ok()))
            .bind(machine.as_str())
            .execute(&self.writer)
            .await
            .map_err(failed)
            .map(drop)
    }
}

fn stored_machine(row: Row, offered: &[Capability]) -> Result<MachineSummary, Fault> {
    let (id, name, os, key, paired_at, core, offers) = row;
    // A value this build can't read shows the machine as not connected yet, not the list as broken.
    let core: Option<App> = core.and_then(|core| serde_json::from_str(&core).ok());
    let offers: Option<Vec<Capability>> =
        offers.and_then(|offers| serde_json::from_str(&offers).ok());
    // Only a core whose build shows can be shown as older than the server.
    let missing = match (&core, offers) {
        (Some(_), Some(offers)) => (offered.iter())
            .filter(|wanted| !offers.contains(wanted))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    Ok(MachineSummary {
        core,
        missing,
        ..paired(id, name, &os, key, paired_at)?
    })
}

#[cfg(test)]
mod tests {
    use weacon_protocol::code;

    use super::*;
    use crate::db::machine::tests::{Setup, claim, confirm, setup, start};
    use crate::db::test_support::{NOW, id};

    #[tokio::test]
    async fn a_machine_shows_its_cores_build_and_what_it_lacks_of_the_servers_offers() {
        let Setup { _dir, db, ada, .. } = setup().await;
        start(&db, "p1", NOW).await;
        assert!(confirm(&db, &ada, "p1", "p1").await);
        claim(&db, "p1", b"secret", NOW + 1).await;
        let offered = [
            Capability::new(code!("machine.connect")),
            Capability::new(code!("machine.settings")),
        ];
        let core = App {
            name: "weacon-core".to_owned(),
            version: "1.2.3".to_owned(),
            commit: "abc".to_owned(),
        };

        let before = db.machines_of(&ada, &offered).await.unwrap();
        db.saw_core(&id("mc-p1"), Some(&core), Some(&offered[..1]))
            .await
            .unwrap();
        let older = db.machines_of(&ada, &offered).await.unwrap();
        let earlier_server = db.machines_of(&ada, &offered[..1]).await.unwrap();
        db.saw_core(&id("mc-p1"), Some(&core), Some(&offered))
            .await
            .unwrap();
        let current = db.machines_of(&ada, &offered).await.unwrap();
        db.saw_core(&id("mc-p1"), Some(&core), None).await.unwrap();
        let unrecorded = db.machines_of(&ada, &offered).await.unwrap();
        db.saw_core(&id("mc-p1"), None, Some(&offered[..1]))
            .await
            .unwrap();
        let unplain = db.machines_of(&ada, &offered).await.unwrap();

        assert_eq!(
            (before[0].core.as_ref(), before[0].missing.len()),
            (None, 0)
        );
        assert_eq!(older[0].core.as_ref(), Some(&core));
        assert_eq!(older[0].missing, offered[1..]);
        assert_eq!(earlier_server[0].missing, []);
        assert_eq!(current[0].missing, []);
        assert_eq!(unrecorded[0].core.as_ref(), Some(&core));
        assert_eq!(unrecorded[0].missing, []);
        assert_eq!(unplain[0].core, None);
        assert_eq!(unplain[0].missing, []);
    }

    #[tokio::test]
    async fn a_stored_build_or_offers_this_build_cant_read_show_as_not_connected() {
        let Setup { _dir, db, ada, .. } = setup().await;
        start(&db, "p1", NOW).await;
        assert!(confirm(&db, &ada, "p1", "p1").await);
        claim(&db, "p1", b"secret", NOW + 1).await;
        sqlx::query("UPDATE machines SET core_build = 'x', core_offers = 'y'")
            .execute(&db.writer)
            .await
            .unwrap();

        let offered = [Capability::new(code!("machine.connect"))];
        let machines = db.machines_of(&ada, &offered).await.unwrap();

        assert_eq!(
            (machines[0].core.as_ref(), machines[0].missing.len()),
            (None, 0)
        );
    }
}
