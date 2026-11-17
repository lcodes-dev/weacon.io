//! This machine's pairing with a server, kept in the state folder: the server, the machine's
//! id, owner and token.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{AccountId, MachineId, code};

use crate::platform;

/// The pairing, as `machine.json` keeps it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Paired {
    /// The server's URL.
    pub(crate) server: String,
    pub(crate) machine: MachineId,
    pub(crate) owner: AccountId,
    pub(crate) token: String,
}

fn file(state_dir: &Path) -> PathBuf {
    state_dir.join("machine.json")
}

/// The pairing, if this machine has one.
///
/// # Errors
/// When the file is there but can't be read.
pub(crate) fn read(state_dir: &Path) -> Result<Option<Paired>, Fault> {
    match fs::read(file(state_dir)) {
        Ok(json) => serde_json::from_slice(&json)
            .map(Some)
            .map_err(|error| unreadable().because(error)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(unreadable().because(error)),
    }
}

/// The pairing as it stands now, read off the async runtime.
///
/// # Errors
/// When there is none, or it can't be read.
pub(crate) async fn current(state_dir: &Path) -> Result<Paired, Fault> {
    let dir = state_dir.to_owned();
    crate::app::blocking(move || Ok(read(&dir)))
        .await
        .map_err(|error| unreadable().because(error))??
        .ok_or_else(|| Fault::new(code!("machine.unpaired"), "This machine isn't paired"))
}

/// Keeps `paired`, in place of any pairing before it.
///
/// # Errors
/// When it can't be written.
pub(crate) fn write(state_dir: &Path, paired: &Paired) -> Result<(), Fault> {
    let json = serde_json::to_vec_pretty(paired).map_err(|error| not_saved().because(error))?;
    platform::own_dir(state_dir).map_err(|error| not_saved().because(error))?;
    weacon_appkit::write_private(&file(state_dir), &json)
        .map_err(|error| not_saved().because(error))
}

/// Forgets `paired` once the server has said the machine is unpaired, unless another pairing
/// has taken its place meanwhile.
///
/// # Errors
/// When the file can't be read, or is there but can't be removed.
pub(crate) fn forget(state_dir: &Path, paired: &Paired) -> Result<(), Fault> {
    let same = read(state_dir)?
        .is_some_and(|kept| kept.machine == paired.machine && kept.token == paired.token);
    if !same {
        return Ok(());
    }
    fs::remove_file(file(state_dir)).map_err(|error| not_saved().because(error))
}

pub(crate) fn unreadable() -> Fault {
    Fault::new(
        code!("machine.unreadable"),
        "This machine's pairing can't be read",
    )
}

fn not_saved() -> Fault {
    Fault::new(
        code!("machine.not_saved"),
        "This machine's pairing couldn't be saved",
    )
}

// The token never reaches a log line, even through `{:?}`.
impl fmt::Debug for Paired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paired")
            .field("machine", &self.machine)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    fn paired() -> Paired {
        Paired {
            server: "https://weacon.example.com".to_owned(),
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: AccountId::try_from("a-1".to_owned()).unwrap(),
            token: "t0ken-marker".to_owned(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_pairing_is_written_for_its_user_alone_and_its_folder_made_so() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        write(&dir, &paired()).unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&dir), mode(&file(&dir))), (0o700, 0o600));
    }

    #[test]
    fn a_pairing_is_kept_read_back_and_forgotten() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");

        assert_eq!(read(&state).unwrap(), None);
        write(&state, &paired()).unwrap();
        let back = read(&state).unwrap();
        forget(&state, &paired()).unwrap();
        forget(&state, &paired()).unwrap();

        assert_eq!(back, Some(paired()));
        assert_eq!(read(&state).unwrap(), None);
    }

    #[test]
    fn a_newer_pairing_isnt_forgotten_for_an_older_one() {
        let dir = TempDir::new().unwrap();
        let mut newer = paired();
        newer.token = "n3wer".to_owned();
        write(&dir, &newer).unwrap();
        let mut other_machine = newer.clone();
        other_machine.machine = MachineId::try_from("mc-2".to_owned()).unwrap();

        forget(&dir, &paired()).unwrap();
        forget(&dir, &other_machine).unwrap();

        assert_eq!(read(&dir).unwrap(), Some(newer));
    }

    #[test]
    fn a_damaged_pairing_is_an_error_not_an_absence() {
        let dir = TempDir::new().unwrap();
        let folder = TempDir::new().unwrap();
        fs::write(file(&dir), "{").unwrap();
        fs::create_dir(file(&folder)).unwrap();

        for state in [&dir, &folder] {
            assert_eq!(
                read(state).unwrap_err().code().as_str(),
                "machine.unreadable"
            );
        }
    }

    #[test]
    fn the_token_stays_out_of_debug() {
        let shown = format!("{:?}", paired());

        assert_eq!(shown, r#"Paired { machine: MachineId("mc-1"), .. }"#);
    }
}
