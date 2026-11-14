//! Machines paired with an account, and pairings under way. Both are server-wide, like
//! accounts, and hold no workspace content.

use sqlx::SqliteConnection;
use subtle::ConstantTimeEq;
use weacon_observability::Fault;
use weacon_protocol::{
    AccountId, MachineId, MachineKey, MachineName, MachineOs, MachineSummary, PairPending,
    PairingId, code,
};

use super::{Db, failed, stored};

/// How long a core has to claim its machine once the person confirms, in seconds.
const CLAIM_WITHIN: i64 = 120;

/// A pairing a core started, with its code and claim secret hashed.
pub(crate) struct NewPairing<'a> {
    pub(crate) id: &'a PairingId,
    pub(crate) code_hash: &'a [u8],
    pub(crate) secret_hash: &'a [u8],
    pub(crate) name: &'a MachineName,
    pub(crate) os: MachineOs,
    pub(crate) key: &'a MachineKey,
    pub(crate) expires_at: i64,
}

/// A person's confirmation of a pairing, which pairs its machine with them as `machine`.
pub(crate) struct Confirmation<'a> {
    pub(crate) account: &'a AccountId,
    pub(crate) pairing: &'a PairingId,
    pub(crate) code_hash: &'a [u8],
    pub(crate) machine: &'a MachineId,
}

/// What a core's claim finds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// No live pairing with that id and secret.
    Unknown,
    Waiting,
    /// Paired with `owner`; the token is now the machine's.
    Paired {
        machine: MachineId,
        owner: AccountId,
    },
}

/// A machine's id, name, system, key and when it was paired, as stored.
type Row = (String, String, String, String, i64);

/// A pairing's secret hash, and once it is confirmed its account, machine, when it was
/// confirmed and whether it was claimed.
type Confirmed = (Vec<u8>, Option<String>, Option<String>, Option<i64>, bool);

impl Db {
    /// Records `pairing`, and ends the expired ones so they don't pile up; `false`, recording
    /// nothing, when `most` are under way already.
    pub(crate) async fn start_pairing(
        &self,
        pairing: &NewPairing<'_>,
        now: i64,
        most: i64,
    ) -> Result<bool, Fault> {
        let mut transaction = self
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        sqlx::query("DELETE FROM pairings WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        let under_way: i64 = sqlx::query_scalar("SELECT count(*) FROM pairings")
            .fetch_one(&mut *transaction)
            .await
            .map_err(failed)?;
        if under_way >= most {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO pairings
                 (pairing_id, code_hash, secret_hash, name, os, machine_key, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(pairing.id.as_str())
        .bind(pairing.code_hash)
        .bind(pairing.secret_hash)
        .bind(pairing.name.as_str())
        .bind(os_text(pairing.os)?)
        .bind(pairing.key.as_str())
        .bind(pairing.expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)?;
        Ok(true)
    }

    /// The live pairing nobody has confirmed whose code is hashed as `code_hash`.
    pub(crate) async fn pending_pairing(
        &self,
        code_hash: &[u8],
        now: i64,
    ) -> Result<Option<PairPending>, Fault> {
        let found = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT pairing_id, name, os, machine_key FROM pairings
             WHERE code_hash = ? AND expires_at > ? AND account_id IS NULL",
        )
        .bind(code_hash)
        .bind(now)
        .fetch_optional(&self.readers)
        .await
        .map_err(failed)?;
        found
            .map(|(pairing, name, os, key)| {
                Ok(PairPending {
                    pairing: stored(pairing)?,
                    name: stored(name)?,
                    os: os_of(&os)?,
                    key: stored(key)?,
                })
            })
            .transpose()
    }

    /// Pairs the machine of the live pairing nobody has confirmed, if the confirmation's code
    /// is the pairing's. The machine is listed once its core claims it.
    pub(crate) async fn confirm_pairing(
        &self,
        confirmation: &Confirmation<'_>,
        now: i64,
    ) -> Result<Option<MachineSummary>, Fault> {
        let Confirmation {
            account,
            pairing,
            code_hash,
            machine,
        } = *confirmation;
        let mut transaction = self
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let Some((stored_hash, name, os, key)) =
            sqlx::query_as::<_, (Vec<u8>, String, String, String)>(
                "SELECT code_hash, name, os, machine_key FROM pairings
                 WHERE pairing_id = ? AND expires_at > ? AND account_id IS NULL",
            )
            .bind(pairing.as_str())
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(failed)?
        else {
            return Ok(None);
        };
        if !bool::from(stored_hash.ct_eq(code_hash)) {
            return Ok(None);
        }
        sqlx::query(
            "UPDATE pairings SET account_id = ?, machine_id = ?, confirmed_at = ?, expires_at = ?
             WHERE pairing_id = ?",
        )
        .bind(account.as_str())
        .bind(machine.as_str())
        .bind(now)
        .bind(now.saturating_add(CLAIM_WITHIN))
        .bind(pairing.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        transaction.commit().await.map_err(failed)?;
        summary((machine.as_str().to_owned(), name, os, key, now)).map(Some)
    }

    /// The core's claim of `pairing` with the secret hashed as `secret_hash`. Once the person
    /// has confirmed, the first claim pairs the machine with the token hashed as `token_hash`,
    /// and later ones until the pairing expires find it paired.
    pub(crate) async fn claim_pairing(
        &self,
        pairing: &PairingId,
        secret_hash: &[u8],
        token_hash: &[u8],
        now: i64,
    ) -> Result<Claim, Fault> {
        let mut transaction = self
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let Some((stored_hash, owner, machine, confirmed_at, claimed)) =
            sqlx::query_as::<_, Confirmed>(
                "SELECT secret_hash, account_id, machine_id, confirmed_at, claimed FROM pairings
             WHERE pairing_id = ? AND expires_at > ?",
            )
            .bind(pairing.as_str())
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(failed)?
        else {
            return Ok(Claim::Unknown);
        };
        if !bool::from(stored_hash.ct_eq(secret_hash)) {
            return Ok(Claim::Unknown);
        }
        let (Some(owner), Some(machine), Some(confirmed_at)) = (owner, machine, confirmed_at)
        else {
            return Ok(Claim::Waiting);
        };
        if !claimed {
            add_machine(&mut transaction, pairing, token_hash, confirmed_at).await?;
        }
        transaction.commit().await.map_err(failed)?;
        Ok(Claim::Paired {
            machine: stored(machine)?,
            owner: stored(owner)?,
        })
    }

    /// The machines paired with `account`, oldest first.
    pub(crate) async fn machines_of(
        &self,
        account: &AccountId,
    ) -> Result<Vec<MachineSummary>, Fault> {
        sqlx::query_as::<_, Row>(
            "SELECT machine_id, name, os, machine_key, paired_at FROM machines
             WHERE account_id = ? ORDER BY paired_at, machine_id",
        )
        .bind(account.as_str())
        .fetch_all(&self.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(summary)
        .collect()
    }

    /// The paired machine whose token is hashed as `token_hash`, and its owner.
    pub(crate) async fn machine_with(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<(MachineId, AccountId)>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT machine_id, account_id FROM machines WHERE token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(&self.readers)
        .await
        .map_err(failed)?
        .map(|(machine, owner)| Ok((stored(machine)?, stored(owner)?)))
        .transpose()
    }

    /// Whether `machine` is paired with `account`.
    pub(crate) async fn has_machine(
        &self,
        account: &AccountId,
        machine: &MachineId,
    ) -> Result<bool, Fault> {
        let found = sqlx::query("SELECT 1 FROM machines WHERE machine_id = ? AND account_id = ?")
            .bind(machine.as_str())
            .bind(account.as_str())
            .fetch_optional(&self.readers)
            .await
            .map_err(failed)?;
        Ok(found.is_some())
    }

    /// Unpairs `machine` from `account`, its pairing too, so it can't be claimed again;
    /// `false` when `account` has no such machine.
    pub(crate) async fn unpair(
        &self,
        account: &AccountId,
        machine: &MachineId,
    ) -> Result<bool, Fault> {
        let mut transaction = self.writer.begin().await.map_err(failed)?;
        let done = sqlx::query("DELETE FROM machines WHERE machine_id = ? AND account_id = ?")
            .bind(machine.as_str())
            .bind(account.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::query("DELETE FROM pairings WHERE machine_id = ? AND account_id = ?")
            .bind(machine.as_str())
            .bind(account.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        transaction.commit().await.map_err(failed)?;
        Ok(done.rows_affected() > 0)
    }
}

/// Pairs the machine of the confirmed `pairing`, with the token hashed as `token_hash`.
async fn add_machine(
    transaction: &mut SqliteConnection,
    pairing: &PairingId,
    token_hash: &[u8],
    paired_at: i64,
) -> Result<(), Fault> {
    sqlx::query(
        "INSERT INTO machines
                 (machine_id, account_id, name, os, machine_key, token_hash, paired_at)
             SELECT machine_id, account_id, name, os, machine_key, ?, ?
             FROM pairings WHERE pairing_id = ?",
    )
    .bind(token_hash)
    .bind(paired_at)
    .bind(pairing.as_str())
    .execute(&mut *transaction)
    .await
    .map_err(failed)?;
    sqlx::query("UPDATE pairings SET claimed = 1 WHERE pairing_id = ?")
        .bind(pairing.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    Ok(())
}

fn summary((id, name, os, key, paired_at): Row) -> Result<MachineSummary, Fault> {
    Ok(MachineSummary {
        id: stored(id)?,
        name: stored(name)?,
        os: os_of(&os)?,
        key: stored(key)?,
        paired_at,
    })
}

fn os_text(os: MachineOs) -> Result<&'static str, Fault> {
    match os {
        MachineOs::Macos => Ok("macos"),
        MachineOs::Linux => Ok("linux"),
        MachineOs::Windows => Ok("windows"),
        MachineOs::Newer => Err(unknown()),
    }
}

fn os_of(text: &str) -> Result<MachineOs, Fault> {
    match text {
        "macos" => Ok(MachineOs::Macos),
        "linux" => Ok(MachineOs::Linux),
        "windows" => Ok(MachineOs::Windows),
        _ => Err(unknown()),
    }
}

fn unknown() -> Fault {
    Fault::new(
        code!("machine.unknown_value"),
        "The machine has a value this version doesn't know",
    )
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::email::Email;
    use crate::test_support::fresh_account;

    const NOW: i64 = 1_762_077_600;

    struct Setup {
        _dir: TempDir,
        db: Db,
        ada: AccountId,
        bo: AccountId,
    }

    async fn setup() -> Setup {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let (ada, bo) = (fresh_account(), fresh_account());
        for (account, email) in [(&ada, "ada@example.com"), (&bo, "bo@example.com")] {
            let email = Email::parse(email).unwrap();
            db.add_account(account, &email).await.unwrap();
        }
        Setup {
            _dir: dir,
            db,
            ada,
            bo,
        }
    }

    fn id<T: TryFrom<String>>(id: &str) -> T {
        T::try_from(id.to_owned()).ok().unwrap()
    }

    fn key() -> MachineKey {
        MachineKey::from_bytes([7; 32])
    }

    // Starts pairing `pairing`, whose code hash is its id and whose secret hash is "secret".
    async fn start(db: &Db, pairing: &str, now: i64, most: i64) -> bool {
        let new = NewPairing {
            id: &id(pairing),
            code_hash: pairing.as_bytes(),
            secret_hash: b"secret",
            name: &id("Studio"),
            os: MachineOs::Linux,
            key: &key(),
            expires_at: now + 600,
        };
        db.start_pairing(&new, now, most).await.unwrap()
    }

    async fn confirm(db: &Db, account: &AccountId, pairing: &str, code: &str) -> bool {
        let confirmation = Confirmation {
            account,
            pairing: &id(pairing),
            code_hash: code.as_bytes(),
            machine: &id(&format!("mc-{pairing}")),
        };
        db.confirm_pairing(&confirmation, NOW + 1)
            .await
            .unwrap()
            .is_some()
    }

    async fn claim(db: &Db, pairing: &str, secret: &[u8], now: i64) -> Claim {
        db.claim_pairing(&id(pairing), secret, b"token", now)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_confirmed_pairing_is_claimed_once_by_its_secret() {
        let Setup { _dir, db, ada, .. } = setup().await;
        assert!(start(&db, "p1", NOW, 10).await);
        let pending = db.pending_pairing(b"p1", NOW).await.unwrap().unwrap();
        let before = claim(&db, "p1", b"secret", NOW).await;
        assert!(!confirm(&db, &ada, "p1", "p2").await);
        assert!(confirm(&db, &ada, "p1", "p1").await);
        let again = confirm(&db, &ada, "p1", "p1").await;
        let gone = db.pending_pairing(b"p1", NOW).await.unwrap();
        let wrong = claim(&db, "p1", b"other", NOW).await;
        let claimed = claim(&db, "p1", b"secret", NOW + 2).await;
        let twice = claim(&db, "p1", b"secret", NOW + 2).await;

        assert_eq!(
            pending,
            PairPending {
                pairing: id("p1"),
                name: id("Studio"),
                os: MachineOs::Linux,
                key: key(),
            }
        );
        assert_eq!(before, Claim::Waiting);
        assert!(!again);
        assert_eq!(gone, None);
        assert_eq!(wrong, Claim::Unknown);
        assert_eq!(
            claimed,
            Claim::Paired {
                machine: id("mc-p1"),
                owner: ada.clone(),
            }
        );
        assert_eq!(twice, claimed);
        let machines = db.machines_of(&ada).await.unwrap();
        assert_eq!(
            machines,
            [MachineSummary {
                id: id("mc-p1"),
                name: id("Studio"),
                os: MachineOs::Linux,
                key: key(),
                paired_at: NOW + 1,
            }]
        );
    }

    #[tokio::test]
    async fn pairings_end_when_they_expire() {
        let Setup { _dir, db, ada, .. } = setup().await;
        assert!(start(&db, "p1", NOW, 10).await);
        assert!(start(&db, "p2", NOW, 10).await);
        assert!(confirm(&db, &ada, "p2", "p2").await);

        assert_eq!(
            db.pending_pairing(b"p1", NOW + 599)
                .await
                .unwrap()
                .map(|pending| pending.pairing),
            Some(id("p1"))
        );
        assert_eq!(db.pending_pairing(b"p1", NOW + 600).await.unwrap(), None);
        assert_eq!(claim(&db, "p1", b"secret", NOW + 600).await, Claim::Unknown);
        assert_eq!(
            claim(&db, "p2", b"secret", NOW + 1 + CLAIM_WITHIN).await,
            Claim::Unknown
        );
        // The next start ends both.
        assert!(start(&db, "p3", NOW + 600, 1).await);
        assert_eq!(db.machines_of(&ada).await.unwrap(), []);
    }

    #[tokio::test]
    async fn only_so_many_pairings_are_under_way() {
        let Setup { _dir, db, .. } = setup().await;

        assert!(start(&db, "p1", NOW, 2).await);
        assert!(start(&db, "p2", NOW, 2).await);
        assert!(!start(&db, "p3", NOW, 2).await);
        assert_eq!(db.pending_pairing(b"p3", NOW).await.unwrap(), None);
        assert!(start(&db, "p3", NOW + 600, 2).await);
    }

    #[tokio::test]
    async fn only_its_owner_sees_and_unpairs_a_machine() {
        let Setup { _dir, db, ada, bo } = setup().await;
        assert!(start(&db, "p1", NOW, 10).await);
        assert!(confirm(&db, &ada, "p1", "p1").await);
        let unclaimed = db.machines_of(&ada).await.unwrap();
        claim(&db, "p1", b"secret", NOW + 2).await;
        let machine = id("mc-p1");

        assert_eq!(unclaimed, []);
        assert!(db.has_machine(&ada, &machine).await.unwrap());
        assert!(!db.has_machine(&bo, &machine).await.unwrap());
        assert_eq!(db.machines_of(&bo).await.unwrap(), []);
        assert!(!db.unpair(&bo, &machine).await.unwrap());
        assert_eq!(db.machines_of(&ada).await.unwrap().len(), 1);
        assert!(db.unpair(&ada, &machine).await.unwrap());
        assert_eq!(db.machines_of(&ada).await.unwrap(), []);
        assert!(!db.has_machine(&ada, &machine).await.unwrap());
        assert!(!db.unpair(&ada, &machine).await.unwrap());
        assert_eq!(claim(&db, "p1", b"secret", NOW + 3).await, Claim::Unknown);
    }

    #[test]
    fn every_known_system_is_stored_and_read_back() {
        for os in [MachineOs::Macos, MachineOs::Linux, MachineOs::Windows] {
            assert_eq!(os_of(os_text(os).unwrap()).unwrap(), os);
        }
        os_text(MachineOs::Newer).unwrap_err();
        os_of("plan9").unwrap_err();
    }
}
