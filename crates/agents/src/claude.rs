//! The driver for Claude Code, through its stream-json mode.

mod wire;

use std::path::PathBuf;

use tokio::process::Command;
use weacon_observability::Fault;
use weacon_protocol::{AgentSession, RunId};

pub(crate) use wire::{Wire, answer, prompt, read};

use crate::run::{self, AgentRun};
use crate::spawn::SpawnPolicy;

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
    /// Starts `claude` under `policy`, resuming `session` when given.
    ///
    /// # Errors
    /// When the program can't be started.
    pub fn start(
        &self,
        policy: &SpawnPolicy,
        run: RunId,
        session: Option<&AgentSession>,
    ) -> Result<AgentRun, Fault> {
        run::spawn(
            self.command(session),
            policy,
            run,
            run::Wire::Claude(Wire::default()),
        )
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
            .current_dir(&self.cwd);
        command
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::test_support::run_id;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weacon-agents-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Real processes: a shell script stands in for `claude`.
    #[cfg(unix)]
    mod side_by_side {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        use tokio::time::timeout;
        use weacon_protocol::{Outcome, RunEvent};

        use super::*;
        use crate::test_support::started;

        const SESSION_A: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
        const SESSION_B: &str = "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a";

        // A `claude` that notes its arguments, reads one line, starts a tool that notes what it
        // inherited, then prints its workspace's session.
        fn fake_claude(dir: &Path) -> PathBuf {
            let program = dir.join("claude");
            fs::write(
                &program,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CLAUDE_CONFIG_DIR/args\"\nread -r line\n\
                 sh -c 'cat /proc/self/oom_score_adj; echo \"$CARGO_BUILD_JOBS $CLAUDE_CONFIG_DIR\"' \
                 > \"$CLAUDE_CONFIG_DIR/tool\" 2>/dev/null\n\
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
        async fn events(mut run: AgentRun) -> Vec<RunEvent> {
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
                events(
                    first
                        .start(&SpawnPolicy::default(), run_id("run-1"), None)
                        .unwrap()
                ),
                events(
                    second
                        .start(
                            &SpawnPolicy::default(),
                            run_id("run-1"),
                            Some(&AgentSession::try_from(SESSION_A.to_owned()).unwrap())
                        )
                        .unwrap()
                )
            );

            let finished = RunEvent::Finished {
                outcome: Outcome::Completed,
            };
            assert_eq!(first_events, [started(SESSION_A), finished.clone()]);
            assert_eq!(second_events, [started(SESSION_B), finished]);
            let args = |name| fs::read_to_string(dir.join(name).join("args")).unwrap();
            assert!(!args("first").contains("--resume"));
            assert!(args("second").ends_with(&format!("--resume\n{SESSION_A}\n")));
            fs::remove_dir_all(&dir).unwrap();
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn tools_inherit_the_policy_but_never_a_changed_config() {
            use std::collections::BTreeMap;

            let dir = temp_dir("policy");
            fake_claude(&dir);
            let claude = workspace(&dir, "first", SESSION_A);
            let policy = SpawnPolicy {
                env: BTreeMap::from([
                    ("CARGO_BUILD_JOBS".into(), "3".into()),
                    ("CLAUDE_CONFIG_DIR".into(), "/elsewhere".into()),
                ]),
                // The highest score, so it raises whatever the test process has.
                oom_score_adj: Some(1000),
            };

            events(claude.start(&policy, run_id("run-1"), None).unwrap()).await;

            let tool = fs::read_to_string(dir.join("first/tool")).unwrap();
            let config = dir.join("first");
            assert_eq!(tool, format!("1000\n3 {}\n", config.display()));
            fs::remove_dir_all(&dir).unwrap();
        }
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

        let Err(error) = claude.start(&SpawnPolicy::default(), run_id("run-1"), None) else {
            panic!("a missing program started");
        };

        assert_eq!(error.code().as_str(), "agent.not_started");
        fs::remove_dir_all(&dir).unwrap();
    }
}
