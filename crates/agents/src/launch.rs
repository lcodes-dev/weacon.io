use std::path::{Path, PathBuf};

use tokio::process::Command;
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, AgentSession, App, RunId, code};

use crate::run::{self, AgentRun};
use crate::spawn::SpawnPolicy;
use crate::{claude, codex};

/// How to start one of the person's own agents for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub agent: AgentKind,
    /// The vendor's installed program.
    pub program: PathBuf,
    /// The workspace's config folder for this agent, which holds its sign-in.
    pub config_dir: PathBuf,
    /// The folder the agent works in.
    pub cwd: PathBuf,
}

impl Launch {
    /// `agent` in `cwd`, with the vendor's program as found on PATH and a config folder named
    /// after it in `workspace_dir`.
    ///
    /// # Errors
    /// When this build doesn't know the agent.
    pub fn new(agent: AgentKind, workspace_dir: &Path, cwd: PathBuf) -> Result<Self, Fault> {
        let program = match agent {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Newer => return Err(unknown()),
        };
        Ok(Self {
            agent,
            program: PathBuf::from(program),
            config_dir: workspace_dir.join(program),
            cwd,
        })
    }

    /// Starts the agent under `policy`, resuming `session` when given. `app` is Weacon's own
    /// build, for agents that record their client.
    ///
    /// # Errors
    /// When this build doesn't know the agent, or its program can't be started.
    pub fn start(
        &self,
        policy: &SpawnPolicy,
        app: &App,
        run: RunId,
        session: Option<&AgentSession>,
    ) -> Result<AgentRun, Fault> {
        let (command, wire) = self.command(app, session)?;
        run::spawn(command, policy, run, wire)
    }

    fn command(
        &self,
        app: &App,
        session: Option<&AgentSession>,
    ) -> Result<(Command, run::Wire), Fault> {
        match self.agent {
            AgentKind::Claude => Ok((
                claude::command(self, session),
                run::Wire::Claude(claude::Wire::default()),
            )),
            AgentKind::Codex => Ok((
                codex::command(self),
                run::Wire::Codex(codex::Wire::new(session.cloned(), app)),
            )),
            AgentKind::Newer => Err(unknown()),
        }
    }
}

fn unknown() -> Fault {
    Fault::new(code!("agent.unknown"), "This build doesn't know that agent")
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{app, run_id};

    fn launch(agent: AgentKind, dir: &TempDir) -> Launch {
        Launch {
            agent,
            program: dir.join("agent"),
            config_dir: dir.join("config"),
            cwd: dir.to_path_buf(),
        }
    }

    fn start(launch: &Launch) -> Result<AgentRun, Fault> {
        launch.start(&SpawnPolicy::default(), &app(), run_id("run-1"), None)
    }

    #[test]
    fn each_agent_starts_through_its_own_driver() {
        let dir = TempDir::new().unwrap();
        let first_arg = |agent| {
            let (command, wire) = launch(agent, &dir).command(&app(), None).unwrap();
            let command = command.as_std();
            assert_eq!(command.get_program(), dir.join("agent"));
            (command.get_args().next().map(ToOwned::to_owned), wire)
        };

        let (claude, claude_wire) = first_arg(AgentKind::Claude);
        let (codex, codex_wire) = first_arg(AgentKind::Codex);

        assert_eq!(claude.as_deref(), Some("--print".as_ref()));
        assert_eq!(codex.as_deref(), Some("app-server".as_ref()));
        assert!(matches!(claude_wire, run::Wire::Claude(_)));
        assert!(matches!(codex_wire, run::Wire::Codex(_)));
    }

    #[test]
    fn a_launch_names_the_vendors_program_and_its_folder_in_the_workspace() {
        let new = |agent| Launch::new(agent, Path::new("/state/w"), PathBuf::from("/work"));
        let claude = new(AgentKind::Claude).unwrap();
        let codex = new(AgentKind::Codex).unwrap();

        assert_eq!(
            claude,
            Launch {
                agent: AgentKind::Claude,
                program: PathBuf::from("claude"),
                config_dir: PathBuf::from("/state/w/claude"),
                cwd: PathBuf::from("/work"),
            }
        );
        assert_eq!(
            (codex.agent, codex.program, codex.config_dir),
            (AgentKind::Codex, "codex".into(), "/state/w/codex".into())
        );
        assert_eq!(
            new(AgentKind::Newer).unwrap_err().code().as_str(),
            "agent.unknown"
        );
    }

    #[tokio::test]
    async fn an_agent_that_cant_start_is_reported() {
        let dir = TempDir::new().unwrap();
        let code = |agent| {
            start(&launch(agent, &dir))
                .err()
                .unwrap()
                .code()
                .as_str()
                .to_owned()
        };

        assert_eq!(code(AgentKind::Claude), "agent.not_started");
        assert_eq!(code(AgentKind::Codex), "agent.not_started");
        assert_eq!(code(AgentKind::Newer), "agent.unknown");
    }
}
