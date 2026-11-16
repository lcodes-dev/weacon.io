//! Whether this machine's owner shares it with a workspace: every member may then call agents
//! on it, and no one else.

use std::io::Write;
use std::path::Path;

use weacon_observability::{Fault, Logger};
use weacon_protocol::{WorkspaceId, code};

use super::{Settings, Workspaces};
use crate::app::{self, App};

impl Workspaces {
    /// The workspaces this machine is shared with, sorted by id; one whose settings can't be
    /// read isn't shared.
    pub(crate) fn shared(&self) -> Vec<WorkspaceId> {
        let mut known = self.known().unwrap_or_default();
        known.retain(|workspace| self.shares(workspace));
        known
    }

    /// Whether this machine is shared with `workspace`.
    pub(crate) fn shares(&self, workspace: &WorkspaceId) -> bool {
        self.settings(workspace)
            .is_ok_and(|settings| settings.shared)
    }
}

/// [`Workspaces::shares`], read off the async runtime; a reading that fails shares nothing.
pub(crate) async fn shares(state_dir: &Path, workspace: &WorkspaceId) -> bool {
    let (workspaces, workspace) = (Workspaces::new(state_dir, None), workspace.clone());
    app::blocking(move || Ok(workspaces.shares(&workspace)))
        .await
        .unwrap_or(false)
}

/// [`Workspaces::shared`], read off the async runtime; a reading that fails shares nothing.
pub(crate) async fn shared(state_dir: &Path) -> Vec<WorkspaceId> {
    let workspaces = Workspaces::new(state_dir, None);
    app::blocking(move || Ok(workspaces.shared()))
        .await
        .unwrap_or_default()
}

/// Saves whether this machine is shared with `workspace`, keeping the rest of its settings.
pub(crate) fn store(
    workspaces: &Workspaces,
    workspace: &WorkspaceId,
    shared: bool,
    log: &Logger,
) -> Result<Settings, Fault> {
    let settings = Settings {
        shared,
        ..workspaces.settings(workspace)?
    };
    workspaces.save(workspace, &settings).map_err(|error| {
        Fault::new(code!("share.not_saved"), "The choice couldn't be saved").because(error)
    })?;
    let fields = [
        ("workspace_id", workspace.as_str().into()),
        ("shared", shared.into()),
    ];
    log.info("sharing set", &fields);
    Ok(settings)
}

/// Shares this machine with `workspace`, or stops, and says which it is now.
pub(crate) fn run(
    app: &App,
    workspace: &WorkspaceId,
    shared: bool,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let workspaces = Workspaces::new(app.state_dir()?, None);
    let settings = store(&workspaces, workspace, shared, &app.log)?;
    let id = workspace.as_str();
    let said = if settings.shared {
        writeln!(
            out,
            "Workspace {id}: shared. Every member can call agents on this machine; you answer \
             their prompts."
        )
    } else {
        writeln!(out, "Workspace {id}: not shared")
    };
    said.map_err(|error| {
        Fault::new(
            code!("share.output_failed"),
            "The choice couldn't be printed",
        )
        .because(error)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::AccessLevel;
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, Broken};

    fn id(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn share(dir: &Path, workspace: &str, shared: bool) -> Result<String, Fault> {
        let app = test_support::app(Some(dir), None);
        let mut out = Vec::new();
        run(&app, &id(workspace), shared, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_machine_is_shared_with_each_workspace_its_owner_says_and_no_other() {
        let dir = TempDir::new().unwrap();
        let workspaces = Workspaces::new(&dir, None);
        assert!(workspaces.shared().is_empty());
        let log = test_support::app(Some(&dir), None).log.clone();
        super::super::store_access(&workspaces, &id("w2"), AccessLevel::Full, None, &log).unwrap();

        let on = share(&dir, "w1", true).unwrap();
        share(&dir, "w3", true).unwrap();

        assert!(on.starts_with("Workspace w1: shared. Every member can call agents"));
        assert_eq!(workspaces.shared(), [id("w1"), id("w3")]);
        assert!(workspaces.shares(&id("w1")) && !workspaces.shares(&id("w2")));
        let off = share(&dir, "w1", false).unwrap();
        assert_eq!(off, "Workspace w1: not shared\n");
        assert_eq!(workspaces.shared(), [id("w3")]);
        assert_eq!(
            fs::read_to_string(dir.join("settings/w-w3.json")).unwrap(),
            r#"{"access":"ask_first","team":false,"shared":true}"#
        );
    }

    #[test]
    fn sharing_keeps_the_access_level_and_a_new_level_keeps_the_sharing() {
        let dir = TempDir::new().unwrap();
        let workspaces = Workspaces::new(&dir, None);
        let log = test_support::app(Some(&dir), None).log.clone();
        let access = |level, team| {
            super::super::store_access(&workspaces, &id("w1"), level, team, &log).unwrap()
        };
        access(AccessLevel::ReadOnly, None);

        let shared = store(&workspaces, &id("w1"), true, &log).unwrap();

        assert_eq!((shared.access, shared.shared), (AccessLevel::ReadOnly, true));
        assert!(access(AccessLevel::Full, None).shared);
        assert!(access(AccessLevel::Full, Some(true)).shared);
    }

    #[tokio::test]
    async fn settings_that_cant_be_read_share_nothing_and_take_no_choice() {
        let dir = TempDir::new().unwrap();
        share(&dir, "w1", true).unwrap();
        fs::write(dir.join("settings/w-w1.json"), "not json").unwrap();

        assert!(!shares(&dir, &id("w1")).await);
        assert!(shared(&dir).await.is_empty());
        let refused = share(&dir, "w1", true).unwrap_err();
        assert_eq!(refused.code().as_str(), "workspace.settings_unreadable");
    }

    #[test]
    fn a_choice_that_cant_be_printed_is_an_error_after_it_is_saved() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);

        let error = run(&app, &id("w1"), true, &mut Broken).unwrap_err();

        assert_eq!(error.code().as_str(), "share.output_failed");
        assert!(Workspaces::new(&dir, None).shares(&id("w1")));
    }
}
