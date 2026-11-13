//! What a workspace's agents may do on this machine, kept in the workspace's folder.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{AccessLevel, WorkspaceId, code};

use super::Workspaces;
use crate::app::{App, state_unavailable};
use crate::platform;

const FILE: &str = "workspace.json";

/// What this machine's owner chose for one workspace; a workspace without a choice asks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) access: AccessLevel,
    /// Whether other people are in the workspace.
    team: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            access: AccessLevel::AskFirst,
            team: false,
        }
    }
}

impl Settings {
    /// The settings kept in a workspace's folder `root`.
    pub(crate) fn read(root: &Path) -> Result<Self, Fault> {
        let unreadable = || {
            Fault::new(
                code!("workspace.settings_unreadable"),
                "The workspace's settings couldn't be read",
            )
        };
        match fs::read(root.join(FILE)) {
            Ok(json) => serde_json::from_slice(&json).map_err(|error| unreadable().because(error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(unreadable().because(error)),
        }
    }

    fn write(self, root: &Path) -> io::Result<()> {
        platform::private_dir(root)?;
        weacon_appkit::write_whole(&root.join(FILE), &serde_json::to_vec(&self)?)
    }

    pub(crate) const fn level(self) -> &'static str {
        level_name(self.access)
    }

    /// A line of warning for full access in a team workspace, or nothing.
    pub(crate) fn warning(self) -> &'static str {
        if self.team && self.access == AccessLevel::Full {
            "Warning: full access is on in a team workspace. Its agents change files on this \
             machine without asking.\n"
        } else {
            ""
        }
    }
}

/// The name `weacon access` takes and shows for `level`; one this build doesn't know allows
/// nothing.
const fn level_name(level: AccessLevel) -> &'static str {
    match level {
        AccessLevel::Off | AccessLevel::Newer => "off",
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

/// Sets what `workspace`'s agents may do on this machine, and says what it is now.
pub(crate) fn run(
    app: &App,
    workspace: &WorkspaceId,
    access: AccessLevel,
    team: Option<bool>,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let root = Workspaces::new(state_dir, None).root(workspace);
    // Saying whether it's a team replaces settings that can't be read.
    let team = match team {
        Some(team) => team,
        None => Settings::read(&root)?.team,
    };
    let settings = Settings { access, team };
    settings.write(&root).map_err(|error| {
        Fault::new(
            code!("access.not_saved"),
            "The access level couldn't be saved",
        )
        .because(error)
    })?;
    let ids = [("workspace_id", workspace.as_str().into())];
    app.log.info("access set", &ids);
    let (id, level, warning) = (workspace.as_str(), settings.level(), settings.warning());
    write!(out, "Workspace {id}: access {level}\n{warning}").map_err(|error| {
        Fault::new(
            code!("access.output_failed"),
            "The access level couldn't be printed",
        )
        .because(error)
    })
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support;

    fn id(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn set(dir: &Path, access: AccessLevel, team: Option<bool>) -> Result<String, Fault> {
        let app = test_support::app(Some(dir), None);
        let mut out = Vec::new();
        run(&app, &id("w1"), access, team, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_workspace_without_a_choice_asks_first_and_a_choice_is_kept() {
        let dir = TempDir::new().unwrap();
        let root = dir.join("workspaces/w-w1");
        assert_eq!(Settings::read(&root).unwrap(), Settings::default());
        assert_eq!(Settings::default().access, AccessLevel::AskFirst);

        let said = set(&dir, AccessLevel::ReadOnly, None).unwrap();

        assert_eq!(said, "Workspace w1: access read-only\n");
        assert_eq!(
            Settings::read(&root).unwrap(),
            Settings {
                access: AccessLevel::ReadOnly,
                team: false
            }
        );
        assert_eq!(
            fs::read_to_string(root.join(FILE)).unwrap(),
            r#"{"access":"read_only","team":false}"#
        );
    }

    #[test]
    fn full_access_in_a_team_workspace_warns_and_the_team_mark_is_kept() {
        let dir = TempDir::new().unwrap();
        let personal = set(&dir, AccessLevel::Full, None).unwrap();
        let team = set(&dir, AccessLevel::Full, Some(true)).unwrap();
        let asking = set(&dir, AccessLevel::AskFirst, None).unwrap();
        let full_again = set(&dir, AccessLevel::Full, None).unwrap();
        let left = set(&dir, AccessLevel::Full, Some(false)).unwrap();

        assert_eq!(personal, "Workspace w1: access full\n");
        assert_eq!(
            team,
            "Workspace w1: access full\nWarning: full access is on in a team workspace. Its \
             agents change files on this machine without asking.\n"
        );
        assert_eq!(asking, "Workspace w1: access ask-first\n");
        assert_eq!(full_again, team);
        assert_eq!(left, personal);
    }

    #[test]
    fn only_full_access_in_a_team_workspace_has_a_warning() {
        let warning = |access, team| Settings { access, team }.warning();

        assert!(warning(AccessLevel::Full, true).starts_with("Warning: full access"));
        assert_eq!(warning(AccessLevel::Full, false), "");
        for quiet in [
            AccessLevel::Off,
            AccessLevel::ReadOnly,
            AccessLevel::AskFirst,
            AccessLevel::Newer,
        ] {
            assert_eq!(warning(quiet, true), "", "{quiet:?}");
        }
    }

    #[test]
    fn older_and_newer_settings_files_still_read() {
        let dir = TempDir::new().unwrap();
        let read = |json: &str| {
            fs::write(dir.join(FILE), json).unwrap();
            Settings::read(&dir)
        };

        assert_eq!(read("{}").unwrap(), Settings::default());
        assert_eq!(
            read(r#"{"access":"full","team":true,"later":1}"#).unwrap(),
            Settings {
                access: AccessLevel::Full,
                team: true
            }
        );
        let newer = read(r#"{"access":"sandboxed"}"#).unwrap();
        assert_eq!(newer.access, AccessLevel::Newer);
        assert_eq!((newer.level(), newer.warning()), ("off", ""));
        let broken = read("not json").unwrap_err();
        assert_eq!(broken.code().as_str(), "workspace.settings_unreadable");
    }

    #[test]
    fn settings_that_cant_be_read_or_saved_are_errors() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.join("workspaces/w-w1").join(FILE)).unwrap();
        let unreadable = set(&dir, AccessLevel::Full, None).unwrap_err();
        let broken = TempDir::new().unwrap();
        fs::create_dir_all(broken.join("workspaces/w-w1")).unwrap();
        fs::write(broken.join("workspaces/w-w1").join(FILE), "").unwrap();
        let kept = set(&broken, AccessLevel::Off, None).unwrap_err();
        let replaced = set(&broken, AccessLevel::Off, Some(true)).unwrap();
        let blocked = TempDir::new().unwrap();
        // A file where the workspaces' folder belongs; the choice in full is never read.
        fs::write(blocked.join("workspaces"), "").unwrap();
        let unsaved = set(&blocked, AccessLevel::Full, Some(false)).unwrap_err();
        let app = test_support::app(None, None);
        let nowhere = run(&app, &id("w1"), AccessLevel::Off, None, &mut Vec::new()).unwrap_err();

        assert_eq!(unreadable.code().as_str(), "workspace.settings_unreadable");
        assert_eq!(unsaved.code().as_str(), "access.not_saved");
        assert_eq!(kept.code().as_str(), "workspace.settings_unreadable");
        assert_eq!(replaced, "Workspace w1: access off\n");
        let full = set(&broken, AccessLevel::Full, None).unwrap();
        assert_eq!(full.lines().count(), 2);
        assert_eq!(nowhere.code().as_str(), "state.unavailable");
    }

    #[test]
    fn a_report_that_cant_be_printed_is_an_error_after_the_level_is_saved() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);

        let error = run(&app, &id("w1"), AccessLevel::Off, None, &mut Closed).unwrap_err();

        assert_eq!(error.code().as_str(), "access.output_failed");
        let saved = Settings::read(&dir.join("workspaces/w-w1")).unwrap();
        assert_eq!(saved.access, AccessLevel::Off);
    }

    #[cfg(unix)]
    #[test]
    fn a_workspaces_folder_made_for_its_settings_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        set(&dir, AccessLevel::Full, None).unwrap();

        for folder in ["workspaces", "workspaces/w-w1"] {
            let mode = fs::metadata(dir.join(folder)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{folder}");
        }
    }
}
