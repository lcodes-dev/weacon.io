//! The driver for Claude Code, through its stream-json mode.

pub(crate) mod hook;
pub(crate) mod transcript;
mod wire;

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use weacon_protocol::{AccessLevel, AgentSession};

pub(crate) use wire::{Wire, answer, prompt, read};

use crate::launch::{Launch, SignInEnv};

/// The tools that only change files, and the input naming each one's file.
const FILE_TOOLS: [(&str, &str); 4] = [
    ("Edit", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
    ("Write", "file_path"),
];

/// Whether `tool` only changes a file.
fn changes_file(tool: &str) -> bool {
    FILE_TOOLS.iter().any(|(name, _)| *name == tool)
}

/// The file a prompt for `tool` would change, when it only changes a file.
pub(crate) fn changed_file(tool: &str, input: &Value) -> Option<PathBuf> {
    let (_, key) = FILE_TOOLS.iter().find(|(name, _)| *name == tool)?;
    input.get(key)?.as_str().map(PathBuf::from)
}

/// The folders and files under home that hold its sign-in.
pub(crate) const CREDENTIALS: [&str; 2] = [".claude", ".claude.json"];

// Its config folder's variable, and the sign-ins and providers that would win over it.
const SIGN_IN_ENV: SignInEnv = SignInEnv {
    config_var: "CLAUDE_CONFIG_DIR",
    prefixes: &["ANTHROPIC_", "CLAUDE_"],
    names: &["AWS_BEARER_TOKEN_BEDROCK"],
};

/// `claude` in its stream-json mode, resuming `session` when given.
pub(crate) fn command(launch: &Launch, session: Option<&AgentSession>) -> Command {
    let mut command = SIGN_IN_ENV.command(launch);
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
    if let Some(hook) = &launch.hook {
        command.args(["--settings", &hook_settings(hook)]);
    }
    if let Some(session) = session {
        command.args(["--resume", session.as_str()]);
    }
    command.current_dir(&launch.cwd);
    command
}

// Settings that run `hook` before every tool; flag settings load with the user's alone, and a
// hook's "ask" outweighs any rule that would allow the call.
fn hook_settings(hook: &[String]) -> String {
    let command: Vec<String> = hook.iter().map(|word| shell_quoted(word)).collect();
    // Claude Code goes on when a hook fails in any other way than exiting with 2.
    let command = format!("{} || exit 2", command.join(" "));
    let hooks = json!([{ "type": "command", "command": command }]);
    json!({ "hooks": { "PreToolUse": [{ "matcher": "*", "hooks": hooks }] } }).to_string()
}

// Claude Code runs a hook's command through a shell, which reads a single-quoted word as is.
fn shell_quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// `claude`'s own check of its sign-in, which exits with success when signed in and prints
/// how; it runs wherever Weacon does.
pub(crate) fn status(launch: &Launch) -> Command {
    let mut command = SIGN_IN_ENV.command(launch);
    command.args(["auth", "status", "--json"]);
    command
}

/// The two fields of `status`'s answer that say where a sign-in comes from; nothing else in it
/// is kept.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    auth_method: Option<String>,
    api_key_source: Option<String>,
}

/// Whether `printed`, what `status` printed, shows a sign-in kept in the config folder: a
/// claude.ai login, or a key `claude auth login` stored. Anything else, such as a host's token,
/// isn't one.
pub(crate) fn own_sign_in(printed: &[u8]) -> bool {
    let Ok(source) = serde_json::from_slice::<Source>(printed) else {
        return false;
    };
    match (
        source.auth_method.as_deref(),
        source.api_key_source.as_deref(),
    ) {
        (Some("claude.ai"), _) => true,
        (Some("api_key"), Some(key)) => key == "/login managed key",
        _ => false,
    }
}

/// `claude`'s own sign-in, for the person's terminal.
pub(crate) fn login(launch: &Launch) -> Command {
    let mut command = SIGN_IN_ENV.command(launch);
    command.args(["auth", "login"]);
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

        use crate::platform::write_program;
        use crate::run::AgentRun;
        use crate::spawn_policy::SpawnPolicy;
        use crate::test_support::{app, run_id, started};

        const SESSION_A: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
        const SESSION_B: &str = "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a";

        // Scripts stand in for `claude auth status`, printing `status` and exiting `code`.
        #[tokio::test]
        async fn only_a_status_that_exits_well_and_names_the_config_folders_sign_in_counts() {
            use crate::launch::SignIn;

            let dir = TempDir::new().unwrap();
            let own = r#"{"loggedIn":true,"authMethod":"claude.ai"}"#;
            let host = r#"{"loggedIn":true,"authMethod":"oauth_token"}"#;
            let padded = |by: usize| format!("{}{own}", " ".repeat(by));
            let script = |number, status: &str, code| status_script(&dir, number, status, code);

            assert_eq!(
                script(0, own, 0).sign_in_status().await.unwrap(),
                SignIn::SignedIn
            );
            assert_eq!(
                script(1, own, 1).sign_in_status().await.unwrap(),
                SignIn::SignedOut
            );
            assert_eq!(
                script(2, host, 0).sign_in_status().await.unwrap(),
                SignIn::SignedOut
            );
            assert_eq!(
                script(3, &padded(4096), 0).sign_in_status().await.unwrap(),
                SignIn::SignedIn
            );
            // Past the limit, so the answer is never read.
            let past = padded(70 * 1024);
            assert_eq!(
                script(4, &past, 0).sign_in_status().await.unwrap(),
                SignIn::SignedOut
            );
        }

        fn status_script(dir: &Path, number: usize, status: &str, code: u8) -> Launch {
            let program = dir.join(format!("claude-{number}"));
            write_program(
                &program,
                &format!("#!/bin/sh\nprintf '%s' '{status}'\nexit {code}\n"),
            );
            Launch {
                agent: AgentKind::Claude,
                program,
                config_dir: dir.to_path_buf(),
                cwd: dir.to_path_buf(),
                access: AccessLevel::AskFirst,
                hook: None,
            }
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
                hook: None,
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
    fn every_command_runs_with_the_workspaces_config() {
        let claude = crate::test_support::launch(AgentKind::Claude, Path::new("/work"));
        let check = status(&claude);
        assert_eq!(
            check.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "status", "--json"]
        );
        assert_eq!(check.as_std().get_current_dir(), None);
        let login = login(&claude);
        assert_eq!(
            login.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "login"]
        );
        for command in [command(&claude, None), check, login] {
            let config = (
                OsStr::new("CLAUDE_CONFIG_DIR"),
                Some(OsStr::new("/work/config")),
            );
            assert!(command.as_std().get_envs().any(|env| env == config));
        }
        let run = command(&claude, None);
        assert_eq!(run.as_std().get_current_dir(), Some(Path::new("/work")));
    }

    #[test]
    fn other_sign_ins_and_providers_are_known_and_the_rest_is_kept() {
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "AWS_BEARER_TOKEN_BEDROCK",
        ] {
            assert!(SIGN_IN_ENV.matches(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "AWS_REGION"] {
            assert!(!SIGN_IN_ENV.matches(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn a_launch_with_a_hook_runs_it_before_every_tool() {
        let words = ["/opt/weacon", "guard", "check", "--worktree=/it's here"];
        let hooked = Launch {
            hook: Some(words.map(str::to_owned).to_vec()),
            ..crate::test_support::launch(AgentKind::Claude, Path::new("/work"))
        };
        let args = |launch: &Launch| -> Vec<String> {
            let command = command(launch, None);
            command
                .as_std()
                .get_args()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect()
        };

        let hooked = args(&hooked);
        let at = hooked.iter().position(|arg| arg == "--settings").unwrap();
        let settings: Value = serde_json::from_str(&hooked[at + 1]).unwrap();
        let hook = &settings["hooks"]["PreToolUse"][0];
        let line = hook["hooks"][0]["command"].as_str().unwrap();

        assert_eq!(hook["matcher"], "*");
        assert_eq!(hook["hooks"][0]["type"], "command");
        assert_eq!(
            line,
            "'/opt/weacon' 'guard' 'check' '--worktree=/it'\\''s here' || exit 2"
        );
        let plain = args(&crate::test_support::launch(
            AgentKind::Claude,
            Path::new("/work"),
        ));
        assert!(!plain.contains(&"--settings".to_owned()));
        // A shell reads the words back as they were.
        if cfg!(unix) {
            let echoed = std::process::Command::new("sh")
                .args(["-c", &format!("printf '%s\\n' {line}")])
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8(echoed.stdout).unwrap(),
                words.join("\n") + "\n"
            );
            // A guard that can't start blocks the tool.
            let missing = std::process::Command::new("sh")
                .args(["-c", line])
                .status()
                .unwrap();
            assert_eq!(missing.code(), Some(2));
        }
    }

    #[test]
    fn every_level_ignores_the_repositorys_settings_and_read_only_refuses_changing_tools() {
        let args = |access| {
            let launch = Launch {
                access,
                ..crate::test_support::launch(AgentKind::Claude, Path::new("/work"))
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
    fn only_a_sign_in_from_the_config_folder_counts() {
        let own = |status: &str| own_sign_in(status.as_bytes());

        assert!(own(
            r#"{"loggedIn":true,"authMethod":"claude.ai","email":"a@example.com"}"#
        ));
        assert!(own(
            r#"{"authMethod":"api_key","apiKeySource":"/login managed key"}"#
        ));
        for other in [
            r#"{"loggedIn":true,"authMethod":"oauth_token"}"#,
            r#"{"authMethod":"api_key","apiKeySource":"ANTHROPIC_API_KEY"}"#,
            r#"{"authMethod":"api_key"}"#,
            r#"{"authMethod":"api_key_helper","apiKeySource":"apiKeyHelper"}"#,
            r#"{"authMethod":"third_party"}"#,
            r#"{"authMethod":"none"}"#,
            r#"{"authMethod":"newer"}"#,
            r#"{"loggedIn":true}"#,
            r#"{"authMethod":1}"#,
            "Logged in",
            "",
        ] {
            assert!(!own(other), "{other}");
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
