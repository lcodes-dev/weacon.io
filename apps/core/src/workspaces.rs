//! What this machine keeps for each workspace: a folder of its own, holding each agent's
//! config folder and so its sign-in.

use std::path::{Path, PathBuf};

use weacon_agents::Launch;
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, WorkspaceId, code};

use crate::platform;

#[derive(Debug)]
pub(crate) struct Workspaces {
    dir: PathBuf,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "runs start their agents through this")
)]
impl Workspaces {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("workspaces"),
        }
    }

    /// The folder for everything this machine keeps for `workspace`.
    pub(crate) fn root(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join(folder_name(workspace))
    }

    /// How to start `agent` in `cwd` with `workspace`'s own config folder, created private when
    /// missing; the only place runs get an agent.
    pub(crate) fn launch(
        &self,
        workspace: &WorkspaceId,
        agent: AgentKind,
        cwd: PathBuf,
    ) -> Result<Launch, Fault> {
        let launch = Launch::new(agent, &self.root(workspace), cwd)?;
        platform::private_dir(&launch.config_dir).map_err(|error| {
            Fault::new(
                code!("workspace.folder_failed"),
                "The workspace's config folder couldn't be created",
            )
            .because(error)
        })?;
        Ok(launch)
    }
}

// A capital becomes `.` and its small letter, so ids differing only in case don't share a
// folder; the prefix keeps Windows' device names, such as `CON`, out.
fn folder_name(workspace: &WorkspaceId) -> String {
    let mut name = String::from("w-");
    for letter in workspace.as_str().chars() {
        if letter.is_ascii_uppercase() {
            name.push('.');
        }
        name.push(letter.to_ascii_lowercase());
    }
    name
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    fn id(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    #[test]
    fn each_workspace_and_agent_gets_its_own_config_folder() {
        let dir = TempDir::new().unwrap();
        let workspaces = Workspaces::new(&dir);
        let launch = |workspace: &str, agent| {
            workspaces
                .launch(&id(workspace), agent, PathBuf::from("/work"))
                .unwrap()
        };

        let claude = launch("w1", AgentKind::Claude);
        let codex = launch("w1", AgentKind::Codex);
        let other = launch("w2", AgentKind::Claude);

        assert_eq!(
            claude,
            Launch {
                agent: AgentKind::Claude,
                program: PathBuf::from("claude"),
                config_dir: dir.join("workspaces/w-w1/claude"),
                cwd: PathBuf::from("/work"),
            }
        );
        assert_eq!(codex.program, PathBuf::from("codex"));
        assert_eq!(codex.config_dir, dir.join("workspaces/w-w1/codex"));
        assert_eq!(other.config_dir, dir.join("workspaces/w-w2/claude"));
        assert_eq!(workspaces.root(&id("w1")), dir.join("workspaces/w-w1"));
        for launch in [claude, codex, other] {
            assert!(launch.config_dir.is_dir());
        }
    }

    #[test]
    fn ids_that_differ_only_in_case_or_name_a_device_get_folders_of_their_own() {
        let names =
            ["ab", "Ab", "aB", "AB", "a-b", "CON", "con"].map(|name| folder_name(&id(name)));

        assert_eq!(
            names,
            [
                "w-ab", "w-.ab", "w-a.b", "w-.a.b", "w-a-b", "w-.c.o.n", "w-con"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_folders_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let launch = Workspaces::new(&dir)
            .launch(&id("w1"), AgentKind::Claude, dir.to_path_buf())
            .unwrap();

        for folder in [launch.config_dir.as_path(), &dir.join("workspaces")] {
            let mode = fs::metadata(folder).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", folder.display());
        }
    }

    #[test]
    fn an_unknown_agent_or_an_unusable_folder_is_refused() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("workspaces"), "").unwrap();
        let workspaces = Workspaces::new(&dir);
        let code = |agent| {
            let error = workspaces
                .launch(&id("w1"), agent, dir.to_path_buf())
                .unwrap_err();
            error.code().as_str().to_owned()
        };

        assert_eq!(code(AgentKind::Newer), "agent.unknown");
        assert_eq!(code(AgentKind::Claude), "workspace.folder_failed");
    }
}
