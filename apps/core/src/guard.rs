//! `weacon guard check`: an agent's pre-tool hook, which asks a person before anything on the
//! guard's list, whatever the agent's own settings allow.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use weacon_agents::guard::{self, Context, Reason};
use weacon_protocol::AgentKind;

use crate::app::App;
use crate::cli::GuardCommand;
use crate::platform::{self, Env};

// Far above any real tool call; the rest isn't read.
const INPUT_LIMIT: u64 = 1 << 20;

// The vendors' own folders and files that hold a sign-in, under the home folder.
const CREDENTIALS: [&str; 4] = [".claude", ".claude.json", ".codex", ".config/gh"];

/// Answers the hook on `input` on `out`, never failing: a hook that fails would block the
/// agent. A call the guard can't read asks only in a run Weacon started.
pub(crate) fn run(
    app: &App,
    command: GuardCommand,
    env: Env<'_>,
    input: &mut impl Read,
    out: &mut impl Write,
) {
    let GuardCommand::Check {
        agent,
        workspace,
        worktree,
        main_checkout,
    } = command;
    if agent != AgentKind::Claude {
        return;
    }
    let mut call = Vec::new();
    let read = input.take(INPUT_LIMIT).read_to_end(&mut call);
    let answer = read.ok().and_then(|_| {
        let context = |cwd: &Path| Context {
            cwd: cwd.to_path_buf(),
            worktree: worktree.clone().unwrap_or_else(|| cwd.to_path_buf()),
            main_checkout: main_checkout.clone(),
            ..protected(app, env)
        };
        guard::claude_hook(&call, context).ok()
    });
    let reason = match answer {
        Some(reason) => reason,
        None => workspace.is_some().then_some(Reason::Unsure),
    };
    if let Some(reason) = reason {
        // Nothing more can be done if the agent stopped reading.
        drop(out.write_all(guard::claude_ask(reason).as_bytes()));
    }
}

// What no agent changes without asking on this machine: Weacon's state and the vendors'
// credentials, among them each workspace's config folders.
fn protected(app: &App, env: Env<'_>) -> Context {
    let home = platform::home_dir(env);
    let mut credentials: Vec<PathBuf> = home
        .iter()
        .flat_map(|home| CREDENTIALS.map(|name| home.join(name)))
        .collect();
    credentials.extend(app.state_dir.iter().map(|state| state.join("workspaces")));
    Context {
        home,
        weacon: app.state_dir.iter().cloned().collect(),
        credentials,
        ..Context::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use weacon_protocol::WorkspaceId;
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, env};

    fn check(app: &App, home: &Path, workspace: Option<&str>, input: &str) -> String {
        let command = GuardCommand::Check {
            agent: AgentKind::Claude,
            workspace: workspace.map(|id| WorkspaceId::try_from(id.to_owned()).unwrap()),
            worktree: None,
            main_checkout: None,
        };
        let home = home.to_str().unwrap();
        let vars = env(&[("HOME", home), ("USERPROFILE", home)]);
        let mut out = Vec::new();
        run(app, command, &vars, &mut input.as_bytes(), &mut out);
        String::from_utf8(out).unwrap()
    }

    fn bash(cwd: &Path, command: &str) -> String {
        json!({ "tool_name": "Bash", "tool_input": { "command": command }, "cwd": cwd }).to_string()
    }

    #[test]
    fn a_listed_operation_asks_and_anything_else_says_nothing() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir.join("state")), None);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let reason = |answer: &str| {
            let answer: Value = serde_json::from_str(answer).unwrap();
            answer["hookSpecificOutput"]["permissionDecisionReason"].clone()
        };

        let outside = check(&app, &dir, None, &bash(&work, "rm -rf /etc"));
        let secret = check(
            &app,
            &dir,
            None,
            &bash(&work, "cat ~/.claude/.credentials.json"),
        );
        let state = format!("echo x > {}", dir.join("state/settings/w.json").display());
        let weacon = check(&app, &dir, None, &bash(&work, &state));
        let config = format!(
            "cat {}",
            dir.join("state/workspaces/w-w1/claude/x").display()
        );
        let config = check(&app, &dir, None, &bash(&work, &config));

        assert_eq!(reason(&outside), Reason::DeleteOutside.text());
        assert_eq!(reason(&secret), Reason::Credentials.text());
        assert_eq!(reason(&weacon), Reason::WeaconFolder.text());
        assert_eq!(reason(&config), Reason::Credentials.text());
        assert_eq!(check(&app, &dir, None, &bash(&work, "cargo test")), "");
    }

    #[test]
    fn a_call_it_cant_read_asks_only_in_a_weacon_run() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);
        let codex = GuardCommand::Check {
            agent: AgentKind::Codex,
            workspace: None,
            worktree: None,
            main_checkout: None,
        };
        let mut out = Vec::new();

        run(&app, codex, &env(&[]), &mut "{}".as_bytes(), &mut out);

        assert!(out.is_empty());
        assert_eq!(check(&app, &dir, None, "not json"), "");
        assert!(check(&app, &dir, Some("w1"), "not json").contains("\"ask\""));
    }
}
