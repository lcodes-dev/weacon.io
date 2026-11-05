//! The driver for Codex, through its app server.

mod wire;

use std::path::PathBuf;

use tokio::process::Command;
use weacon_observability::Fault;
use weacon_protocol::{AgentSession, App, RunId};

pub(crate) use wire::{Wire, answer};

use crate::run::{self, AgentRun};

/// How to start the person's own `codex` for one workspace.
#[derive(Clone, Debug)]
pub struct Codex {
    /// The installed `codex`.
    pub program: PathBuf,
    /// The workspace's config folder, which holds its sign-in.
    pub config_dir: PathBuf,
    /// The folder the agent works in.
    pub cwd: PathBuf,
    /// Weacon's own build, which Codex records as its client.
    pub app: App,
}

// Sign-ins that would win over the workspace's own; removed, never read.
const OTHER_SIGN_INS: [&str; 2] = ["CODEX_API_KEY", "OPENAI_API_KEY"];

impl Codex {
    /// Starts `codex app-server` and opens a thread, resuming `session` when given. Answers
    /// reach a prompt by `run`, so the caller must never give two runs the same id.
    ///
    /// # Errors
    /// When the program can't be started.
    pub fn start(&self, run: RunId, session: Option<&AgentSession>) -> Result<AgentRun, Fault> {
        let mut wire = Wire::new(session.cloned());
        let opening = wire.opening(&self.app);
        run::spawn(self.command(), run, run::Wire::Codex(wire), opening)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.arg("app-server");
        for name in OTHER_SIGN_INS {
            command.env_remove(name);
        }
        command
            .env("CODEX_HOME", &self.config_dir)
            .current_dir(&self.cwd);
        command
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::*;
    use crate::test_support::run_id;

    fn codex(dir: &Path) -> Codex {
        Codex {
            program: dir.join("codex"),
            config_dir: dir.join("config"),
            cwd: dir.to_owned(),
            app: App {
                name: "weacon-core".to_owned(),
                version: "0.1.0".to_owned(),
                commit: "1a2b3c4".to_owned(),
            },
        }
    }

    #[test]
    fn the_app_server_runs_with_the_workspaces_config_and_no_other_sign_in() {
        let command = codex(Path::new("/work")).command();
        let command = command.as_std();
        let envs: Vec<_> = command.get_envs().collect();

        assert_eq!(command.get_args().collect::<Vec<_>>(), ["app-server"]);
        assert!(envs.contains(&(OsStr::new("CODEX_HOME"), Some(OsStr::new("/work/config")))));
        for name in OTHER_SIGN_INS {
            assert!(envs.contains(&(OsStr::new(name), None)), "{name}");
        }
        assert_eq!(command.get_current_dir(), Some(Path::new("/work")));
    }

    #[tokio::test]
    async fn a_missing_program_is_reported() {
        let dir = std::env::temp_dir().join(format!("weacon-codex-{}", std::process::id()));

        let Err(error) = codex(&dir).start(run_id("run-1"), None) else {
            panic!("a missing program started");
        };

        assert_eq!(error.code().as_str(), "agent.not_started");
    }
}
