//! The driver for Claude Code, through its stream-json mode.

pub(crate) mod transcript;
mod wire;

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use weacon_protocol::{AccessLevel, AgentSession, App};

pub(crate) use wire::{Wire, answer, prompt, read};

use crate::launch::{Launch, McpServer, OtherSignIns};
use crate::vendor::Vendor;
use crate::wire::Wire as RunWire;

pub(crate) const VENDOR: Vendor = Vendor {
    config_variable: "CLAUDE_CONFIG_DIR",
    other_sign_ins: OtherSignIns {
        prefixes: &["ANTHROPIC_", "CLAUDE_"],
        names: &["AWS_BEARER_TOKEN_BEDROCK"],
    },
    status: &["auth", "status", "--json"],
    login: &["auth", "login"],
    own_sign_in,
    takes_mcp: true,
    drive,
    resume: Some(resume),
};

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

const TOKEN_VARIABLE: &str = "WEACON_MCP_TOKEN";
const TOKEN_HEADER: &str = "Bearer ${WEACON_MCP_TOKEN}";

/// `claude` in its stream-json mode, resuming `session` when given.
fn drive(launch: &Launch, _: &App, session: Option<&AgentSession>) -> (Command, RunWire) {
    (command(launch, session), RunWire::Claude(Wire::default()))
}

fn command(launch: &Launch, session: Option<&AgentSession>) -> Command {
    let mut command = launch.with_workspace_sign_in(&VENDOR);
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
    if let Some(mcp) = &launch.mcp {
        point_at(&mut command, mcp);
    }
    // An absolute path can't be read as an option.
    if let Some(plugin) = launch.plugin.as_ref().filter(|plugin| plugin.is_absolute()) {
        command.arg("--plugin-dir").arg(plugin);
    }
    if let Some(session) = session {
        command.args(["--resume", session.as_str()]);
    }
    command.current_dir(&launch.cwd);
    command
}

/// Points `command` at the core's MCP, whose read-only tools then need no person.
fn point_at(command: &mut Command, mcp: &McpServer) {
    // Claude reads the token from its environment, so it never reaches the command line.
    let server =
        json!({"type": "http", "url": mcp.url, "headers": {"Authorization": TOKEN_HEADER}});
    let config = json!({"mcpServers": {"weacon": server}});
    command.args(["--mcp-config", &config.to_string()]);
    let tools: Vec<_> = mcp
        .tools
        .iter()
        .map(|tool| format!("mcp__weacon__{tool}"))
        .collect();
    command.args(["--allowedTools", &tools.join(",")]);
    command.env(TOKEN_VARIABLE, &mcp.token);
}

/// The two fields of `claude auth status --json`'s answer that say where a sign-in comes
/// from; nothing else in it is kept.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    auth_method: Option<String>,
    api_key_source: Option<String>,
}

/// Whether the status printed a sign-in kept in the config folder: a claude.ai login, or a key
/// that `claude auth login` stored. Anything else, such as a token from the host, isn't one.
fn own_sign_in(status: &[u8]) -> bool {
    let Ok(source) = serde_json::from_slice::<Source>(status) else {
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

/// `claude` on `session`, for a person to carry on by hand in their terminal, in the folder the
/// chat works in. Weacon's own limits are left out: the person answers its prompts.
fn resume(launch: &Launch, session: &AgentSession) -> Command {
    let mut command = launch.with_workspace_sign_in(&VENDOR);
    command.args(["--resume", session.as_str()]);
    command.current_dir(&launch.cwd);
    command
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::launch::McpServer;
    use crate::test_support::launch;
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
        use weacon_testing::platform::write_program;

        use crate::run::AgentRun;
        use crate::spawn::SpawnPolicy;
        use crate::test_support::{OTHER_SESSION, SESSION, app, run_id, started};

        // Scripts stand in for `claude auth status`, printing `status` and exiting `code`.
        #[tokio::test]
        async fn only_a_status_that_exits_well_and_names_the_config_folders_sign_in_counts() {
            use crate::launch::SignIn;

            let dir = TempDir::new().unwrap();
            let own = r#"{"loggedIn":true,"authMethod":"claude.ai"}"#;
            let host = r#"{"loggedIn":true,"authMethod":"oauth_token"}"#;
            let padded = |by: usize| format!("{}{own}", " ".repeat(by));
            let script = |number, status: &str, code| status_script(&dir, number, status, code);

            assert_eq!(script(0, own, 0).sign_in().await.unwrap(), SignIn::SignedIn);
            assert_eq!(
                script(1, own, 1).sign_in().await.unwrap(),
                SignIn::SignedOut
            );
            assert_eq!(
                script(2, host, 0).sign_in().await.unwrap(),
                SignIn::SignedOut
            );
            assert_eq!(
                script(3, &padded(4096), 0).sign_in().await.unwrap(),
                SignIn::SignedIn
            );
            // Past the limit, so the answer is never read.
            let past = padded(70 * 1024);
            assert_eq!(
                script(4, &past, 0).sign_in().await.unwrap(),
                SignIn::SignedOut
            );
        }

        fn status_script(dir: &Path, number: usize, status: &str, code: u8) -> Launch {
            let program = dir.join(format!("claude-{number}"));
            write_program(
                &program,
                &format!("#!/bin/sh\nprintf '%s' '{status}'\nexit {code}\n"),
            )
            .unwrap();
            Launch {
                config_dir: dir.to_path_buf(),
                ..launch(AgentKind::Claude, program, dir)
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
            )
            .unwrap();
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
                config_dir,
                ..launch(AgentKind::Claude, dir.join("claude"), dir)
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
            let first = workspace(&dir, "first", SESSION);
            let second = workspace(&dir, "second", OTHER_SESSION);

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
                            Some(&AgentSession::try_from(SESSION.to_owned()).unwrap())
                        )
                        .unwrap()
                )
            );

            let finished = RunEvent::Finished {
                outcome: Outcome::Completed,
            };
            assert_eq!(first_events, [started(SESSION), finished.clone()]);
            assert_eq!(second_events, [started(OTHER_SESSION), finished]);
            let args = |name| fs::read_to_string(dir.join(name).join("args")).unwrap();
            assert!(!args("first").contains("--resume"));
            assert!(args("second").ends_with(&format!("--resume\n{SESSION}\n")));
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn tools_inherit_the_policy_but_never_a_changed_config() {
            use std::collections::BTreeMap;

            let dir = TempDir::new().unwrap();
            fake_claude(&dir);
            let claude = workspace(&dir, "first", SESSION);
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
        let claude = launch(AgentKind::Claude, "claude", Path::new("/work"));
        let check = claude.status_command(&VENDOR);
        assert_eq!(
            check.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "status", "--json"]
        );
        assert_eq!(check.as_std().get_current_dir(), None);
        let login = claude.login_command(&VENDOR);
        assert_eq!(
            login.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "login"]
        );
        let session = AgentSession::try_from("0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d".to_owned());
        let by_hand = resume(&claude, &session.unwrap());
        assert_eq!(
            by_hand.as_std().get_args().collect::<Vec<_>>(),
            ["--resume", "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d"]
        );
        assert_eq!(by_hand.as_std().get_current_dir(), Some(Path::new("/work")));
        for command in [command(&claude, None), check, login, by_hand] {
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
            assert!(VENDOR.other_sign_ins.matches(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "AWS_REGION"] {
            assert!(!VENDOR.other_sign_ins.matches(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn every_level_ignores_the_repositorys_settings_and_read_only_refuses_changing_tools() {
        let args = |access| {
            let launch = Launch {
                access,
                ..launch(AgentKind::Claude, "claude", Path::new("/work"))
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

    fn launched(mcp: Option<McpServer>, plugin: Option<PathBuf>) -> (Command, Vec<String>) {
        let launch = Launch {
            access: AccessLevel::ReadOnly,
            mcp,
            plugin,
            ..launch(AgentKind::Claude, "claude", Path::new("/work"))
        };
        let command = command(&launch, None);
        let args = command.as_std().get_args();
        let args = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
        (command, args)
    }

    fn token(command: &Command) -> Option<&OsStr> {
        let mut envs = command.as_std().get_envs();
        envs.find(|(key, _)| *key == "WEACON_MCP_TOKEN")
            .and_then(|(_, value)| value)
    }

    #[test]
    fn a_run_reaches_the_cores_mcp_with_its_token_only_in_its_environment() {
        let server = McpServer {
            url: "http://127.0.0.1:9/mcp".into(),
            token: "secret-token".into(),
            tools: vec!["map".into(), "symbol".into()],
        };
        let (with, given) = launched(Some(server), None);
        let after = |flag: &str| {
            let at = given.iter().position(|arg| arg == flag)?;
            given.get(at + 1).cloned()
        };

        let config: Value = serde_json::from_str(&after("--mcp-config").unwrap()).unwrap();
        let expected = json!({"mcpServers": {"weacon": {
            "type": "http",
            "url": "http://127.0.0.1:9/mcp",
            "headers": {"Authorization": "Bearer ${WEACON_MCP_TOKEN}"},
        }}});
        assert_eq!(config, expected);
        let allowed = after("--allowedTools");
        assert_eq!(
            allowed.as_deref(),
            Some("mcp__weacon__map,mcp__weacon__symbol")
        );
        assert!(!given.iter().any(|arg| arg.contains("secret-token")));
        assert_eq!(token(&with), Some(OsStr::new("secret-token")));
    }

    #[test]
    fn a_run_without_the_mcp_gets_neither_its_flags_nor_a_token() {
        let (without, plain) = launched(None, None);

        let flags = ["--mcp-config", "--allowedTools"];
        assert!(!plain.iter().any(|arg| flags.contains(&arg.as_str())));
        assert_eq!(token(&without), None);
    }

    #[test]
    fn only_an_absolute_plugin_folder_is_given_to_claude() {
        let plugin = std::env::temp_dir().join("weacon-plugin");
        let (_, given) = launched(None, Some(plugin.clone()));
        let at = given.iter().position(|arg| arg == "--plugin-dir").unwrap();
        assert_eq!(given.get(at + 1).map(PathBuf::from), Some(plugin));

        for plugin in [
            None,
            Some(PathBuf::from("--plugin")),
            Some(PathBuf::from("plugin")),
        ] {
            let (_, given) = launched(None, plugin);
            assert!(!given.iter().any(|arg| arg.contains("plugin")), "{given:?}");
        }
    }

    #[test]
    fn an_mcp_servers_debug_hides_its_token() {
        let server = McpServer {
            url: "http://127.0.0.1:9/mcp".into(),
            token: "secret-token".into(),
            tools: vec!["map".into(), "symbol".into()],
        };
        assert_eq!(
            format!("{server:?}"),
            r#"McpServer { url: "http://127.0.0.1:9/mcp", tools: ["map", "symbol"], .. }"#
        );
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
