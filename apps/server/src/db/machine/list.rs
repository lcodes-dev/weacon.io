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
