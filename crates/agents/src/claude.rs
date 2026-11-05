//! The driver for Claude Code, through its stream-json mode.

mod run;
mod wire;

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::BufReader;
use tokio::process::{ChildStdin, ChildStdout, Command};
use weacon_observability::Fault;
use weacon_protocol::{AgentSession, RunId, code};

pub use run::Run;

/// How to start the person's own `claude` for one workspace.
#[derive(Clone, Debug)]
pub struct Claude {
    /// The installed `claude`.
    pub program: PathBuf,
    /// The workspace's config folder, which holds its sign-in.
    pub config_dir: PathBuf,
    /// The folder the agent works in.
    pub cwd: PathBuf,
}

/// A run of a real `claude` process.
pub type ClaudeRun = Run<ChildStdin, BufReader<ChildStdout>>;

// Sign-ins and providers that would win over the workspace's own; removed, never read.
const OTHER_SIGN_INS: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_VERTEX",
];

impl Claude {
    /// Starts `claude` as run `run`, resuming `session` when given.
    ///
    /// # Errors
    /// When the program can't be started.
    pub fn start(&self, run: RunId, session: Option<&AgentSession>) -> Result<ClaudeRun, Fault> {
        let not_started = || Fault::new(code!("agent.not_started"), "The agent couldn't start");
        let mut child = self
            .command(session)
            .spawn()
            .map_err(|error| not_started().because(error))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(not_started());
        };
        Ok(Run::new(run, stdin, BufReader::new(stdout), Some(child)))
    }

    fn command(&self, session: Option<&AgentSession>) -> Command {
        let mut command = Command::new(&self.program);
        command.args([
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
        ]);
        if let Some(session) = session {
            command.args(["--resume", session.as_str()]);
        }
        for name in OTHER_SIGN_INS {
            command.env_remove(name);
        }
        command
            .env("CLAUDE_CONFIG_DIR", &self.config_dir)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;

    use tokio::time::timeout;

    use weacon_protocol::{Outcome, RunEvent};

    use super::*;

    const SESSION_A: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
    const SESSION_B: &str = "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a";

    fn run_id() -> RunId {
        RunId::try_from("run-1".to_owned()).unwrap()
    }

    fn session(id: &str) -> AgentSession {
        AgentSession::try_from(id.to_owned()).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weacon-agents-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A `claude` that notes its arguments, reads one line, then prints its workspace's session.
    fn fake_claude(dir: &Path) -> PathBuf {
        let program = dir.join("claude");
        fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CLAUDE_CONFIG_DIR/args\"\nread -r line\n\
             cat \"$CLAUDE_CONFIG_DIR/session\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    fn workspace(dir: &Path, name: &str, session: &str) -> Claude {
        let config_dir = dir.join(name);
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("session"),
            format!(
                "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{session}\"}}\n\
                 {{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}}\n"
            ),
        )
        .unwrap();
        Claude {
            program: dir.join("claude"),
            config_dir,
            cwd: dir.to_owned(),
        }
    }

    // Bounded in count and time, so a driver that never ends fails instead of hanging.
    async fn events(mut run: ClaudeRun) -> Vec<RunEvent> {
        run.send("Hello").await.unwrap();
        let mut events = Vec::new();
        while events.len() < 10
            && let Some(event) = timeout(Duration::from_secs(10), run.next())
                .await
                .unwrap()
                .unwrap()
        {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn two_workspaces_run_side_by_side_each_with_its_own_config() {
        let dir = temp_dir("workspaces");
        fake_claude(&dir);
        let first = workspace(&dir, "first", SESSION_A);
        let second = workspace(&dir, "second", SESSION_B);

        let (first_events, second_events) = tokio::join!(
            events(first.start(run_id(), None).unwrap()),
            events(second.start(run_id(), Some(&session(SESSION_A))).unwrap())
        );

        let finished = RunEvent::Finished {
            outcome: Outcome::Completed,
        };
        for (events, id) in [(first_events, SESSION_A), (second_events, SESSION_B)] {
            assert_eq!(
                events,
                [
                    RunEvent::Started {
                        session: session(id)
                    },
                    finished.clone()
                ]
            );
        }
        let args = |name| fs::read_to_string(dir.join(name).join("args")).unwrap();
        assert!(!args("first").contains("--resume"));
        assert!(args("second").ends_with(&format!("--resume\n{SESSION_A}\n")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_workspaces_config_replaces_any_other_sign_in() {
        let claude = Claude {
            program: PathBuf::from("claude"),
            config_dir: PathBuf::from("/work/config"),
            cwd: PathBuf::from("/work"),
        };
        let command = claude.command(None);
        let envs: Vec<_> = command.as_std().get_envs().collect();

        assert!(envs.contains(&(
            OsStr::new("CLAUDE_CONFIG_DIR"),
            Some(OsStr::new("/work/config"))
        )));
        for name in OTHER_SIGN_INS {
            assert!(envs.contains(&(OsStr::new(name), None)), "{name}");
        }
        assert_eq!(command.as_std().get_current_dir(), Some(Path::new("/work")));
    }

    #[tokio::test]
    async fn a_missing_program_is_reported() {
        let dir = temp_dir("missing");
        let claude = Claude {
            program: dir.join("claude"),
            config_dir: dir.clone(),
            cwd: dir.clone(),
        };

        let Err(error) = claude.start(run_id(), None) else {
            panic!("a missing program started");
        };

        assert_eq!(error.code().as_str(), "agent.not_started");
        fs::remove_dir_all(&dir).unwrap();
    }
}
