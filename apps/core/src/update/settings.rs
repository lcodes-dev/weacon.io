//! What this machine's owner chose for updates, and the newest release the core has seen.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{ReleaseVersion, UpdateSchedule, code};

use crate::platform;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) schedule: UpdateSchedule,
    /// When the newest manifest seen was published, so an older one can't be passed off as
    /// the latest.
    pub(crate) last_published: u64,
    /// When the daemon last looked for a release.
    pub(crate) last_checked: u64,
    /// The newest release the daemon found and didn't install.
    pub(crate) offered: Option<ReleaseVersion>,
}

pub(crate) fn file(state_dir: &Path) -> PathBuf {
    state_dir.join("settings/update.json")
}

impl Settings {
    /// The settings in `file`; the defaults when there's no file.
    pub(crate) fn read(file: &Path) -> Result<Self, Fault> {
        let unreadable = || {
            Fault::new(
                code!("update.settings_unreadable"),
                "The update settings couldn't be read",
            )
        };
        match fs::read(file) {
            Ok(json) => serde_json::from_slice(&json).map_err(|error| unreadable().because(error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(unreadable().because(error)),
        }
    }

    pub(crate) fn write(self, file: &Path) -> Result<(), Fault> {
        let write = || {
            if let Some(folder) = file.parent() {
                platform::private_dir(folder)?;
            }
            weacon_appkit::write_whole(file, &serde_json::to_vec(&self)?)
        };
        write().map_err(|error| {
            Fault::new(
                code!("update.settings_unwritable"),
                "The update settings couldn't be saved",
            )
            .because(error)
        })
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn settings_start_at_notify_and_keep_what_is_written() {
        let dir = TempDir::new().unwrap();
        let file = file(&dir);
        assert_eq!(Settings::read(&file).unwrap(), Settings::default());
        assert_eq!(Settings::default().schedule, UpdateSchedule::Notify);
        let chosen = Settings {
            schedule: UpdateSchedule::Weekly,
            last_published: 7,
            last_checked: 8,
            offered: Some("1.2.3".parse().unwrap()),
        };
        chosen.write(&file).unwrap();
        assert_eq!(Settings::read(&file).unwrap(), chosen);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            r#"{"schedule":"weekly","last_published":7,"last_checked":8,"offered":"1.2.3"}"#
        );
    }

    #[test]
    fn broken_settings_are_an_error_not_the_defaults() {
        let dir = TempDir::new().unwrap();
        let file = file(&dir);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not json").unwrap();
        let fault = Settings::read(&file).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.settings_unreadable");
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        let fault = Settings::read(&file).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.settings_unreadable");
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let dir = TempDir::new().unwrap();
        let file = file(&dir);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"schedule":"off"}"#).unwrap();
        let read = Settings::read(&file).unwrap();
        assert_eq!(read.schedule, UpdateSchedule::Off);
        assert_eq!(read.last_published, 0);
        assert_eq!(read.last_checked, 0);
    }
}
