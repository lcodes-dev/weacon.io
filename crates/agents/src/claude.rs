//! The driver for Claude Code, through its stream-json mode.

mod wire;

use std::path::PathBuf;

use serde_json::Value;
use tokio::process::Command;
use weacon_protocol::{AccessLevel, AgentSession};

pub(crate) use wire::{Wire, answer, prompt, read};

use crate::launch::Launch;

/// The tools that only change files, and the input naming each one's file.
const FILE_TOOLS: [(&str, &str); 4] = [
    ("Edit", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
    ("Write", "file_path"),
];

/// The file a prompt for `tool` would change, when it only changes a file.
pub(crate) fn changed_file(tool: &str, input: &Value) -> Option<PathBuf> {
    let (_, key) = FILE_TOOLS.iter().find(|(name, _)| *name == tool)?;
    input.get(key)?.as_str().map(PathBuf::from)
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

/// `claude` in its stream-json mode, resuming `session` when given.
pub(crate) fn command(launch: &Launch, session: Option<&AgentSession>) -> Command {
    let mut command = with_workspace_sign_in(launch);
    command.args([
        "--print",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
        // Only the workspace's own settings: a repository's could allow tools or add hooks.
        "--setting-sources",
        "user",
        // So no setting's mode skips the prompts the access level answers.
        "--permission-mode",
        "default",
    ]);
    if launch.access == AccessLevel::ReadOnly {
        let tools = FILE_TOOLS.map(|(tool, _)| tool);
        command.args(["--disallowedTools", &format!("Bash,{}", tools.join(","))]);
    }
    if let Some(session) = session {
        command.args(["--resume", session.as_str()]);
    }
    command.current_dir(&launch.cwd);
    command
}

/// `claude`'s own check of its sign-in, which exits with success when signed in; it runs
/// wherever Weacon does.
pub(crate) fn status(launch: &Launch) -> Command {
    let mut command = with_workspace_sign_in(launch);
    command.args(["auth", "status"]);
    command
}

/// `claude`'s own sign-in, for the person's terminal.
pub(crate) fn login(launch: &Launch) -> Command {
    let mut command = with_workspace_sign_in(launch);
    command.args(["auth", "login"]);
    command
}

fn with_workspace_sign_in(launch: &Launch) -> Command {
    let mut command = Command::new(&launch.program);
    for name in OTHER_SIGN_INS {
        command.env_remove(name);
    }
    command.env("CLAUDE_CONFIG_DIR", &launch.config_dir);
    command
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::*;
    use weacon_protocol::AgentKind;

    // Real processes: a shell script stands in for `claude`.
    #[cfg(unix)]
    mod side_by_side {
        use std::fs;
        use std::time::Duration;

        use tokio::time::timeout;
        use weacon_protocol::{Outcome, RunEvent};

        use super::*;
        use weacon_testing::TempDir;

        use crate::run::AgentRun;
        use crate::spawn::SpawnPolicy;
        use crate::test_support::{app, run_id, started};

        const SESSION_A: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
        const SESSION_B: &str = "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a";

        /// Writes an executable `script` to `path` from a child shell, so this process never holds a
        /// write handle that another test's fork could inherit, which makes running it fail as busy.
        fn write_program(path: &Path, script: &str) {
            let written = std::process::Command::new("sh")
                .args([
                    "-c",
                    "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
                    "sh",
                    script,
                ])
                .arg(path)
                .status()
                .unwrap();
            assert!(written.success());
        }

        // A `claude` that notes its arguments, reads one line, starts a tool that notes what it
        // inherited, then prints its workspace's session.
        fn fake_claude(dir: &Path) -> PathBuf {
            let program = dir.join("claude");
            write_program(
                &program,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CLAUDE_CONFIG_DIR/args\"\nread -r line\n\
                 sh -c 'cat /proc/self/oom_score_adj; echo \"$CARGO_BUILD_JOBS $CLAUDE_CONFIG_DIR\"' \
                 > \"$CLAUDE_CONFIG_DIR/tool\" 2>/dev/null\n\
                 cat \"$CLAUDE_CONFIG_DIR/session\"\n",
            );
            program
        }

        fn workspace(dir: &Path, name: &str, session: &str) -> Launch {
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
            Launch {
                agent: AgentKind::Claude,
                program: dir.join("claude"),
                config_dir,
                cwd: dir.to_owned(),
                access: AccessLevel::AskFirst,
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
            let dir = TempDir::new().unwrap();
            fake_claude(&dir);
            let first = workspace(&dir, "first", SESSION_A);
            let second = workspace(&dir, "second", SESSION_B);

            let (first_events, second_events) = tokio::join!(
                events(
                    first
                        .start(&SpawnPolicy::default(), &app(), run_id("run-1"), None)
                        .unwrap()
                ),
                events(
                    second
                        .start(
                            &SpawnPolicy::default(),
                            &app(),
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
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn tools_inherit_the_policy_but_never_a_changed_config() {
            use std::collections::BTreeMap;

            let dir = TempDir::new().unwrap();
            fake_claude(&dir);
            let claude = workspace(&dir, "first", SESSION_A);
            // Writing its own score back shows whether this procfs takes scores at all.
            let own = fs::read_to_string("/proc/self/oom_score_adj").ok();
            let adjustable = own
                .as_ref()
                .is_some_and(|own| fs::write("/proc/self/oom_score_adj", own).is_ok());
            let policy = SpawnPolicy {
                env: BTreeMap::from([
                    ("CARGO_BUILD_JOBS".into(), "3".into()),
                    ("CLAUDE_CONFIG_DIR".into(), "/elsewhere".into()),
                ]),
                // The highest score, so it raises whatever the test process has.
                oom_score_adj: adjustable.then_some(1000),
            };

            events(
                claude
                    .start(&policy, &app(), run_id("run-1"), None)
                    .unwrap(),
            )
            .await;

            let tool = fs::read_to_string(dir.join("first/tool")).unwrap();
            let config = dir.join("first");
            let raised = String::from("1000\n");
            let score = adjustable.then_some(raised).or(own);
            let expected = format!("{}3 {}\n", score.unwrap_or_default(), config.display());
            assert_eq!(tool, expected);
        }
    }

    #[test]
    fn the_workspaces_config_replaces_any_other_sign_in() {
        let claude = Launch {
            agent: AgentKind::Claude,
            program: PathBuf::from("claude"),
            config_dir: PathBuf::from("/work/config"),
            cwd: PathBuf::from("/work"),
            access: AccessLevel::AskFirst,
        };
        let check = status(&claude);
        assert_eq!(
            check.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "status"]
        );
        assert_eq!(check.as_std().get_current_dir(), None);
        let login = login(&claude);
        assert_eq!(
            login.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "login"]
        );
        for command in [command(&claude, None), check, login] {
            let envs: Vec<_> = command.as_std().get_envs().collect();

            assert!(envs.contains(&(
                OsStr::new("CLAUDE_CONFIG_DIR"),
                Some(OsStr::new("/work/config"))
            )));
            for name in OTHER_SIGN_INS {
                assert!(envs.contains(&(OsStr::new(name), None)), "{name}");
            }
        }
        let run = command(&claude, None);
        assert_eq!(run.as_std().get_current_dir(), Some(Path::new("/work")));
    }

    #[test]
    fn every_level_ignores_the_repositorys_settings_and_read_only_refuses_changing_tools() {
        let args = |access| {
            let launch = Launch {
                agent: AgentKind::Claude,
                program: PathBuf::from("claude"),
                config_dir: PathBuf::from("/work/config"),
                cwd: PathBuf::from("/work"),
                access,
            };
            let command = command(&launch, None);
            let args: Vec<_> = command.as_std().get_args().map(ToOwned::to_owned).collect();
            args.join(OsStr::new(" ")).into_string().unwrap()
        };
        let refused = "--disallowedTools Bash,Edit,MultiEdit,NotebookEdit,Write";

        for access in [
            AccessLevel::AskFirst,
            AccessLevel::Full,
            AccessLevel::ReadOnly,
        ] {
            let args = args(access);
            assert!(args.contains("--setting-sources user"), "{access:?}");
            assert!(args.contains("--permission-mode default"), "{access:?}");
            assert_eq!(args.contains(refused), access == AccessLevel::ReadOnly);
        }
    }

    #[test]
    fn only_file_tools_name_the_file_they_change() {
        let edit = serde_json::json!({ "file_path": "src/lib.rs", "notebook_path": "a.ipynb" });

        for tool in ["Edit", "MultiEdit", "Write"] {
            assert_eq!(changed_file(tool, &edit), Some(PathBuf::from("src/lib.rs")));
        }
        assert_eq!(
            changed_file("NotebookEdit", &edit),
            Some(PathBuf::from("a.ipynb"))
        );
        for (tool, input) in [
            ("Bash", edit.clone()),
            ("Read", edit),
            ("Write", serde_json::json!({ "file_path": 1 })),
            ("Write", serde_json::json!({})),
        ] {
            assert_eq!(changed_file(tool, &input), None, "{tool} {input}");
        }
    }
}
