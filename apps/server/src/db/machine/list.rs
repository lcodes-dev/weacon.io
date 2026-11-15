//! The machines paired with an account, each with its core's build as it last connected.

use weacon_observability::Fault;
use weacon_protocol::{AccountId, App, Capability, MachineId, MachineSummary, code};

use super::paired;
use crate::db::{Db, failed, unreadable};

/// A machine's id, name, system, key and when it was paired, and its core's name, version,
/// commit and offers when it last connected, as stored.
type Row = (
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl Db {
    /// The machines paired with `account`, oldest first, each with what of `offered`, the
    /// server's offers to cores, its core didn't offer back when it last connected.
    pub(crate) async fn machines_of(
        &self,
        account: &AccountId,
        offered: &[Capability],
    ) -> Result<Vec<MachineSummary>, Fault> {
        sqlx::query_as::<_, Row>(
            "SELECT machine_id, name, os, machine_key, paired_at,
                    core_name, core_version, core_commit, core_offers FROM machines
             WHERE account_id = ? ORDER BY paired_at, machine_id",
        )
        .bind(account.as_str())
        .fetch_all(&self.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|row| stored_machine(row, offered))
        .collect()
    }

    /// Keeps the build `core` of `machine` and what it `offers`, as it said when it connected.
    pub(crate) async fn saw_core(
        &self,
        machine: &MachineId,
        core: &App,
        offers: &[Capability],
    ) -> Result<(), Fault> {
        let offers = serde_json::to_string(offers).map_err(|error| {
            Fault::new(code!("db.unwritable"), "A value can't be stored").because(error)
        })?;
        sqlx::query(
            "UPDATE machines SET core_name = ?, core_version = ?, core_commit = ?, core_offers = ?
             WHERE machine_id = ?",
        )
        .bind(&core.name)
        .bind(&core.version)
        .bind(&core.commit)
        .bind(offers)
        .bind(machine.as_str())
        .execute(&self.writer)
        .await
        .map_err(failed)
        .map(drop)
    }
}

fn stored_machine(row: Row, offered: &[Capability]) -> Result<MachineSummary, Fault> {
    let (id, name, os, key, paired_at, core_name, version, commit, offers) = row;
    let core = match (core_name, version, commit) {
        (Some(name), Some(version), Some(commit)) => Some(App {
            name,
            version,
            commit,
        }),
        _ => None,
    };
    let offers: Vec<Capability> = match offers {
        Some(offers) => serde_json::from_str(&offers).map_err(|_| unreadable())?,
        None => Vec::new(),
    };
    let missing = match core {
        Some(_) => (offered.iter())
            .filter(|wanted| !offers.contains(wanted))
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    Ok(MachineSummary {
        core,
        missing,
        ..paired(id, name, &os, key, paired_at)?
    })
}
