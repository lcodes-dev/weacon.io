//! What a workspace's agents may do on this machine, kept apart from its folder.

use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use weacon_observability::{Fault, Logger};
use weacon_protocol::{AccessLevel, WorkspaceId, code};

use super::Workspaces;
use crate::app::{App, read_json};
use crate::platform;

/// What this machine's owner chose for one workspace; a workspace without a choice asks first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) access: AccessLevel,
    /// Whether every member of the workspace may call agents on this machine.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) shared: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            access: AccessLevel::AskFirst,
            shared: false,
        }
    }
}

impl Settings {
    /// The settings in `file`; the defaults when there's no file.
    pub(crate) fn read(file: &Path) -> Result<Self, Fault> {
        let unreadable = || {
            Fault::new(
                code!("workspace.settings_unreadable"),
                "The workspace's settings couldn't be read",
            )
        };
        read_json(file)
            .map(Option::unwrap_or_default)
            .map_err(|error| unreadable().because(error))
    }

    /// Saves the settings in `file`, making its folder private to this user.
    pub(crate) fn write(&self, file: &Path) -> io::Result<()> {
        if let Some(folder) = file.parent() {
            platform::private_dir(folder)?;
        }
        weacon_appkit::write_whole(file, &serde_json::to_vec(&self)?)
    }

    pub(crate) const fn level(&self) -> &'static str {
        level_name(self.access)
    }

    /// What the line of a workspace shared with its members adds after its level.
    pub(crate) const fn sharing(&self) -> &'static str {
        if self.shared { ", shared" } else { "" }
    }
}

/// The name `weacon access` takes and shows for `level`.
pub(crate) const fn level_name(level: AccessLevel) -> &'static str {
    match level {
        AccessLevel::Off => "off",
        AccessLevel::ReadOnly => "read-only",
        AccessLevel::AskFirst => "ask-first",
        AccessLevel::Full => "full",
    }
}

/// The level `weacon access` takes by `name`.
pub(crate) fn parse_level(name: &str) -> Result<AccessLevel, &'static str> {
    [
        AccessLevel::Off,
        AccessLevel::ReadOnly,
        AccessLevel::AskFirst,
        AccessLevel::Full,
    ]
    .into_iter()
    .find(|level| level_name(*level) == name)
    .ok_or("not an access level: use off, read-only, ask-first or full")
}

/// Saves what `workspace`'s agents may do on this machine, in place of settings that can't
/// be read.
pub(crate) fn store(
    workspaces: &Workspaces,
    workspace: &WorkspaceId,
    access: AccessLevel,
    log: &Logger,
) -> Result<Settings, Fault> {
    let unsaved = |error| {
        Fault::new(
            code!("access.not_saved"),
            "The access level couldn't be saved",
        )
        .because(error)
    };
    let _hold = workspaces.hold_settings(workspace).map_err(unsaved)?;
    let current = workspaces.settings(workspace);
    let settings = Settings {
        access,
        ..current.unwrap_or_default()
    };
    workspaces.save(workspace, &settings).map_err(unsaved)?;
    log.info("access set", &[("workspace_id", workspace.as_str().into())]);
    Ok(settings)
}

/// Sets what `workspace`'s agents may do on this machine and says what it is now.
pub(crate) fn run(
    app: &App,
    workspace: &WorkspaceId,
    access: AccessLevel,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let workspaces = Workspaces::new(app.state_dir()?, None);
    let settings = store(&workspaces, workspace, access, &app.log)?;
    let (id, level) = (workspace.as_str(), settings.level());
    writeln!(out, "Workspace {id}: access {level}").map_err(|error| {
        Fault::new(
            code!("access.output_failed"),
            "The access level couldn't be printed",
        )
        .because(error)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, Broken};

    fn id(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    const FILE: &str = "settings/w-w1.json";

    fn set(dir: &Path, access: AccessLevel) -> Result<String, Fault> {
        let app = test_support::app(Some(dir), None);
        let mut out = Vec::new();
        run(&app, &id("w1"), access, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_workspace_without_a_choice_asks_first_and_a_choice_is_kept() {
        let dir = TempDir::new().unwrap();
        let file = dir.join(FILE);
        assert_eq!(Settings::read(&file).unwrap(), Settings::default());
        assert_eq!(Settings::default().access, AccessLevel::AskFirst);

        let said = set(&dir, AccessLevel::ReadOnly).unwrap();

        assert_eq!(said, "Workspace w1: access read-only\n");
        assert_eq!(
            Settings::read(&file).unwrap(),
            Settings {
                access: AccessLevel::ReadOnly,
                ..Settings::default()
            }
        );
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            r#"{"access":"read_only"}"#
        );
        // Nothing an agent is given holds it.
        assert_eq!(
            fs::read_dir(dir.join("workspaces/w-w1")).unwrap().count(),
            0
        );
    }

    #[test]
    fn a_settings_file_reads_with_fields_left_out_or_unknown_but_not_an_unknown_level() {
        let dir = TempDir::new().unwrap();
        let read = |json: &str| {
            let file = dir.join("w-w1.json");
            fs::write(&file, json).unwrap();
            Settings::read(&file)
        };

        assert_eq!(read("{}").unwrap(), Settings::default());
        assert_eq!(
            read(r#"{"access":"full","team":true,"later":1}"#).unwrap(),
            Settings {
                access: AccessLevel::Full,
                shared: false,
            }
        );
        for broken in ["not json", r#"{"access":"sandboxed"}"#] {
            let unreadable = read(broken).unwrap_err();
            assert_eq!(unreadable.code().as_str(), "workspace.settings_unreadable");
        }
    }

    #[test]
    fn settings_that_cant_be_read_are_replaced_and_ones_that_cant_be_saved_are_errors() {
        let broken = TempDir::new().unwrap();
        fs::create_dir_all(broken.join("settings")).unwrap();
        fs::write(broken.join(FILE), "").unwrap();
        let replaced = set(&broken, AccessLevel::Off).unwrap();
        let blocked = TempDir::new().unwrap();
        // A file where the workspaces' folder belongs.
        fs::write(blocked.join("workspaces"), "").unwrap();
        let unsaved = set(&blocked, AccessLevel::Full).unwrap_err();
        let app = test_support::app(None, None);
        let nowhere = run(&app, &id("w1"), AccessLevel::Off, &mut Vec::new()).unwrap_err();

        assert_eq!(replaced, "Workspace w1: access off\n");
        assert_eq!(unsaved.code().as_str(), "access.not_saved");
        assert_eq!(nowhere.code().as_str(), "state.unavailable");
    }

    #[test]
    fn a_report_that_cant_be_printed_is_an_error_after_the_level_is_saved() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);

        let error = run(&app, &id("w1"), AccessLevel::Off, &mut Broken).unwrap_err();

        assert_eq!(error.code().as_str(), "access.output_failed");
        let saved = Settings::read(&dir.join(FILE)).unwrap();
        assert_eq!(saved.access, AccessLevel::Off);
    }

    #[cfg(unix)]
    #[test]
    fn the_folders_made_for_a_workspaces_settings_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        set(&dir, AccessLevel::Full).unwrap();

        for folder in ["workspaces", "workspaces/w-w1", "settings"] {
            let mode = fs::metadata(dir.join(folder)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{folder}");
        }
    }
}
