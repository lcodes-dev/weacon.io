//! `weacon guard check`: Claude Code's pre-tool hook, which asks a person before anything on
//! the guard's list, whatever the agent's own settings allow.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use weacon_agents::guard::{self, Context, Reason};
use weacon_observability::Fault;
use weacon_protocol::{WorkspaceId, code};

use crate::app::{App, state_unavailable};
use crate::cli::{CheckCommand, GuardCommand};
use crate::platform::{self, Env};
use crate::workspaces::Workspaces;

// Far above any real tool call; the rest isn't read.
const INPUT_LIMIT: u64 = 1 << 20;

/// Runs a `weacon guard` command; `input` is the hook's call for `check`.
///
/// # Errors
/// When a workspace's additions can't be read, saved or printed; `check` never fails.
pub(crate) fn run(
    app: &App,
    command: GuardCommand,
    env: Env<'_>,
    input: &mut impl Read,
    out: &mut impl Write,
) -> Result<(), Fault> {
    match command {
        GuardCommand::Check(hook) => {
            check(app, &hook, env, input, out);
            Ok(())
        }
        GuardCommand::Add { workspace, words } => edit(app, &workspace, Some((words, true)), out),
        GuardCommand::Remove { workspace, words } => {
            edit(app, &workspace, Some((words, false)), out)
        }
        GuardCommand::List { workspace } => edit(app, &workspace, None, out),
    }
}

/// Answers the hook on `input` on `out`, never failing, since the hook's command blocks the
/// agent when it fails. A call the guard can't read asks only in a run Weacon started.
fn check(
    app: &App,
    hook: &CheckCommand,
    env: Env<'_>,
    input: &mut impl Read,
    out: &mut impl Write,
) {
    let mut call = Vec::new();
    let read = input.take(INPUT_LIMIT).read_to_end(&mut call);
    let answer = read.ok().and_then(|_| {
        let context = |cwd: &Path| {
            let worktree = hook.worktree.clone().unwrap_or_else(|| cwd.to_path_buf());
            Context {
                cwd: cwd.to_path_buf(),
                main_checkout: main_checkout(&worktree),
                worktree,
                additions: hook
                    .workspace
                    .as_ref()
                    .map(|workspace| added(app, workspace))
                    .unwrap_or_default(),
                ..protected(app, env)
            }
        };
        guard::claude_hook(&call, context).ok()
    });
    let reason = match answer {
        Some(reason) => reason,
        None => hook.workspace.is_some().then_some(Reason::Unsure),
    };
    if let Some(reason) = reason {
        // Nothing more can be done if the agent stopped reading.
        drop(out.write_all(guard::claude_ask(reason).as_bytes()));
    }
}

// The checkout a git worktree was added from: its `.git` file names a folder inside the main
// checkout's `.git`. A folder with a `.git` folder of its own is a main checkout.
fn main_checkout(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Some(worktree.to_path_buf());
    }
    let pointer = fs::read_to_string(dot_git).ok()?;
    let git_dir = PathBuf::from(pointer.strip_prefix("gitdir:")?.trim());
    let git_dir = worktree.join(git_dir);
    git_dir
        .ancestors()
        .find(|folder| folder.file_name().is_some_and(|name| name == ".git"))?
        .parent()
        .map(Path::to_path_buf)
}

// The workspace's additions; none when its settings can't be read.
fn added(app: &App, workspace: &WorkspaceId) -> Vec<Vec<String>> {
    let Some(state_dir) = app.state_dir.as_deref() else {
        return Vec::new();
    };
    Workspaces::new(state_dir, None)
        .settings(workspace)
        .map(|settings| settings.additions)
        .unwrap_or_default()
}

// Adds or removes `change`'s words, saves the list if that changed it, and prints it.
fn edit(
    app: &App,
    workspace: &WorkspaceId,
    change: Option<(Vec<String>, bool)>,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let workspaces = Workspaces::new(state_dir, None);
    let mut settings = workspaces.settings(workspace)?;
    if let Some((words, add)) = change {
        let before = settings.additions.clone();
        settings.additions.retain(|known| *known != words);
        if add {
            settings.additions.push(words);
        }
        if settings.additions != before {
            let file = workspaces.settings_file(workspace);
            let saved = platform::private_dir(&workspaces.root(workspace))
                .and_then(|()| settings.write(&file));
            saved.map_err(|error| {
                Fault::new(
                    code!("guard.not_saved"),
                    "The guard's list couldn't be saved",
                )
                .because(error)
            })?;
            app.log.info(
                "guard changed",
                &[("workspace_id", workspace.as_str().into())],
            );
        }
    }
    let id = workspace.as_str();
    let lines: Vec<String> = if settings.additions.is_empty() {
        vec![format!(
            "Workspace {id}: asks only about the guard's own list"
        )]
    } else {
        let asks = |words: &Vec<String>| format!("Workspace {id}: asks before {}", words.join(" "));
        settings.additions.iter().map(asks).collect()
    };
    writeln!(out, "{}", lines.join("\n")).map_err(|error| {
        Fault::new(
            code!("guard.output_failed"),
            "The guard's list couldn't be printed",
        )
        .because(error)
    })
}

// What no agent changes without asking on this machine: Weacon's state and the vendors'
// credentials, among them each workspace's config folders.
fn protected(app: &App, env: Env<'_>) -> Context {
    let home = platform::home_dir(env);
    let mut credentials = home
        .as_deref()
        .map(guard::credential_folders)
        .unwrap_or_default();
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
        let command = GuardCommand::Check(CheckCommand {
            workspace: workspace.map(|id| WorkspaceId::try_from(id.to_owned()).unwrap()),
            worktree: None,
        });
        let home = home.to_str().unwrap();
        let vars = env(&[("HOME", home), ("USERPROFILE", home)]);
        let mut out = Vec::new();
        run(app, command, &vars, &mut input.as_bytes(), &mut out).unwrap();
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
        fs::create_dir_all(&work).unwrap();
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
        assert_eq!(check(&app, &dir, None, "not json"), "");
        assert!(check(&app, &dir, Some("w1"), "not json").contains("\"ask\""));
    }

    fn change(app: &App, command: GuardCommand) -> Result<String, Fault> {
        let mut out = Vec::new();
        run(app, command, &env(&[]), &mut "".as_bytes(), &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn id(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn words(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn a_workspace_adds_and_removes_commands_to_ask_about() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir), None);
        let add = |text: &str| GuardCommand::Add {
            workspace: id("w1"),
            words: words(text),
        };

        let empty = change(
            &app,
            GuardCommand::List {
                workspace: id("w1"),
            },
        )
        .unwrap();
        change(&app, add("terraform destroy")).unwrap();
        let both = change(&app, add("kubectl delete")).unwrap();
        let again = change(&app, add("terraform destroy")).unwrap();
        let removed = GuardCommand::Remove {
            workspace: id("w1"),
            words: words("terraform destroy"),
        };
        let left = change(&app, removed).unwrap();
        crate::workspaces::access::run(
            &app,
            &id("w1"),
            weacon_protocol::AccessLevel::Full,
            None,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(
            empty,
            "Workspace w1: asks only about the guard's own list\n"
        );
        assert_eq!(
            both,
            "Workspace w1: asks before terraform destroy\nWorkspace w1: asks before kubectl delete\n"
        );
        assert_eq!(again.lines().count(), 2);
        assert_eq!(left, "Workspace w1: asks before kubectl delete\n");
        let kept = change(
            &app,
            GuardCommand::List {
                workspace: id("w1"),
            },
        )
        .unwrap();
        assert_eq!(kept, left);
        assert!(dir.join("workspaces/w-w1").is_dir());
    }

    #[test]
    fn a_hook_in_a_weacon_run_asks_about_its_workspaces_additions() {
        let dir = TempDir::new().unwrap();
        let app = test_support::app(Some(&dir.join("state")), None);
        let work = dir.join("work");
        fs::create_dir_all(&work).unwrap();
        let add = GuardCommand::Add {
            workspace: id("w1"),
            words: words("terraform destroy"),
        };
        change(&app, add).unwrap();
        let call = bash(&work, "terraform destroy -auto-approve");

        assert!(check(&app, &dir, Some("w1"), &call).contains(Reason::Addition.text()));
        assert_eq!(check(&app, &dir, Some("w2"), &call), "");
        assert_eq!(check(&app, &dir, None, &call), "");
    }

    #[test]
    fn additions_that_cant_be_read_saved_or_printed_are_errors() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.join("settings/w-w1.json")).unwrap();
        fs::write(dir.join("workspaces"), "").unwrap();
        let app = test_support::app(Some(&dir), None);
        let code = |result: Result<String, Fault>| result.unwrap_err().code().as_str().to_owned();
        let list = |workspace: &str| GuardCommand::List {
            workspace: id(workspace),
        };
        let mut closed = Closed;

        let unreadable = code(change(&app, list("w1")));
        let unsaved = code(change(
            &app,
            GuardCommand::Add {
                workspace: id("w2"),
                words: words("x"),
            },
        ));
        let unprinted = run(&app, list("w2"), &env(&[]), &mut "".as_bytes(), &mut closed);
        let nowhere = code(change(&test_support::app(None, None), list("w1")));

        assert_eq!(unreadable, "workspace.settings_unreadable");
        assert_eq!(unsaved, "guard.not_saved");
        assert_eq!(
            unprinted.unwrap_err().code().as_str(),
            "guard.output_failed"
        );
        assert_eq!(nowhere, "state.unavailable");
    }

    #[test]
    fn a_worktree_leads_to_the_checkout_it_was_added_from() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.join("main/.git/worktrees/chat")).unwrap();
        fs::create_dir_all(dir.join("chat")).unwrap();
        fs::create_dir_all(dir.join("plain")).unwrap();
        let pointer = format!(
            "gitdir: {}\n",
            dir.join("main/.git/worktrees/chat").display()
        );
        fs::write(dir.join("chat/.git"), pointer).unwrap();
        fs::write(dir.join("plain/.git"), "nothing").unwrap();

        assert_eq!(main_checkout(&dir.join("chat")), Some(dir.join("main")));
        assert_eq!(main_checkout(&dir.join("main")), Some(dir.join("main")));
        assert_eq!(main_checkout(&dir.join("plain")), None);
        assert_eq!(main_checkout(&dir.join("missing")), None);
    }
}
