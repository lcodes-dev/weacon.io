//! Runs one turn of a coding agent's own program in a folder, and tells what it says and
//! does in words that are the same for every agent. The vendors' flags, their sandboxes
//! and what they print are known here and nowhere else.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use core_link::{Agent, Deed, Event};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// The environment variables an agent's program gets: enough to be found and signed in.
pub const KEPT: [&str; 5] = ["PATH", "HOME", "USER", "CODEX_HOME", "CLAUDE_CONFIG_DIR"];

/// Claude Code's own sandbox for commands, with nothing let out of it.
const CLAUDE_SANDBOX: &str = r#"{"sandbox":{"enabled":true,"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":false}}"#;

/// How an agent's program is started for a turn. Each may change files in the folder and run
/// commands inside the vendor's sandbox, which keeps writes to the folder and has no network.
/// Nothing asks a person: what would is refused. None of the person's own setup is loaded.
fn args(agent: Agent, session: Option<&str>) -> Vec<&str> {
    match agent {
        Agent::Claude => {
            let mut args = vec![
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
                "--safe-mode",
                "--strict-mcp-config",
                "--permission-mode",
                "acceptEdits",
                "--permission-prompts",
                "none",
                // Every command is allowed, since every command is in the sandbox.
                "--allowedTools",
                "Bash",
                "--settings",
                CLAUDE_SANDBOX,
            ];
            if let Some(session) = session {
                args.extend(["--resume", session]);
            }
            args
        }
        Agent::Codex => {
            let mut args = vec!["exec"];
            if let Some(session) = session {
                args.extend(["resume", session]);
            }
            args.extend([
                "--json",
                "--config",
                r#"sandbox_mode="workspace-write""#,
                "--ignore-user-config",
                "--ignore-rules",
                "--skip-git-repo-check",
                "-",
            ]);
            args
        }
    }
}

/// How long a program that was told to stop has to end what it started.
const GRACE: Duration = Duration::from_secs(5);

/// Runs one turn of `agent` in `folder` and calls `tell` with what it says and does, as it
/// comes. Returns the session to go on with next time, or why the turn failed. When `stop`
/// ends, so does the program, and the commands it was running with it. `program` is
/// `agent.program()`, or a test's stand-in for it.
pub async fn turn(
    program: &str,
    agent: Agent,
    folder: &Path,
    session: Option<&str>,
    prompt: &str,
    stop: impl Future<Output = ()>,
    mut tell: impl FnMut(Event),
) -> Result<String, String> {
    let child = Command::new(program)
        .args(args(agent, session))
        .current_dir(folder)
        .env_clear()
        .envs(std::env::vars().filter(|(name, _)| KEPT.contains(&name.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = child.map_err(|error| format!("{program} could not be started: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let prompt = prompt.to_owned();
    // Written on the side, so a program that prints before it has read everything can't block us.
    tokio::spawn(async move { stdin.write_all(prompt.as_bytes()).await });
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let complaint = tokio::spawn(async move {
        let mut complaint = String::new();
        stderr.read_to_string(&mut complaint).await.ok();
        complaint
    });

    // The programs name files by the folder's real path, which a link in it would hide.
    let folder = std::fs::canonicalize(folder).unwrap_or(folder.to_owned());
    let folder = format!("{}/", folder.to_string_lossy());
    let mut lines = BufReader::new(child.stdout.take().expect("stdout was piped")).lines();
    let mut session = session.map(str::to_owned);
    let mut ended = None;
    let session = &mut session;
    let mut stop = std::pin::pin!(stop);
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = &mut stop => {
                // Interrupted as Ctrl-C would, each program ends its commands before itself. Killed, it leaves them running.
                if let Some(id) = child.id() {
                    let interrupt = Command::new("kill").args(["-INT", &id.to_string()]).status();
                    interrupt.await.ok();
                }
                if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
                    child.kill().await.ok();
                }
                return Err("Stopped".to_owned());
            }
        };
        let Ok(Some(line)) = line else { break };
        let Ok(printed) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let read = match agent {
            Agent::Claude => claude(&printed, &folder, session, &mut tell),
            Agent::Codex => codex(&printed, &folder, session, &mut tell),
        };
        ended = read.or(ended);
    }
    let left = child.wait().await.map_err(|error| error.to_string())?;
    match (ended, session.take()) {
        (Some(Ok(())), Some(session)) => Ok(session),
        (Some(Err(why)), _) => Err(why),
        _ => {
            let complaint = complaint.await.unwrap_or_default();
            // The end of it says what went wrong.
            let tail = complaint.trim().char_indices().rev().nth(399);
            let tail = &complaint.trim()[tail.map_or(0, |(at, _)| at)..];
            Err(format!(
                "{program} stopped before the turn ended ({left}): {tail}"
            ))
        }
    }
}

/// How a line that ends the turn says it ended: well, or with why not.
type Ended = Option<Result<(), String>>;

/// One line of `claude --output-format stream-json`.
fn claude(
    printed: &Value,
    folder: &str,
    session: &mut Option<String>,
    tell: &mut impl FnMut(Event),
) -> Ended {
    match printed["type"].as_str() {
        // What a sub-agent says and does is its own, and isn't shown yet.
        Some("assistant") if printed["parent_tool_use_id"].is_null() => {
            let parts = printed["message"]["content"].as_array();
            for part in parts.into_iter().flatten() {
                let input = &part["input"];
                let event = match (part["type"].as_str(), part["name"].as_str()) {
                    (Some("text"), _) => said(&part["text"], folder),
                    (Some("tool_use"), Some("Bash")) => did(Deed::Command, &input["command"], ""),
                    (Some("tool_use"), Some("Read")) => {
                        did(Deed::Read, &input["file_path"], folder)
                    }
                    (Some("tool_use"), Some("Edit" | "Write" | "NotebookEdit")) => {
                        let path = input["file_path"]
                            .as_str()
                            .or(input["notebook_path"].as_str());
                        did(Deed::Edit, &path.into(), folder)
                    }
                    (Some("tool_use"), Some(name)) => did(Deed::Other, &name.into(), ""),
                    _ => None,
                };
                event.into_iter().for_each(&mut *tell);
            }
        }
        Some("result") => {
            *session = printed["session_id"].as_str().map(str::to_owned);
            let why = text(&printed["result"]).unwrap_or("Claude Code failed");
            return Some(match printed["is_error"].as_bool() {
                Some(true) => Err(why.to_owned()),
                _ => Ok(()),
            });
        }
        _ => {}
    }
    None
}

/// One line of `codex exec --json`.
fn codex(
    printed: &Value,
    folder: &str,
    session: &mut Option<String>,
    tell: &mut impl FnMut(Event),
) -> Ended {
    let item = &printed["item"];
    match (printed["type"].as_str(), item["type"].as_str()) {
        (Some("thread.started"), _) => {
            *session = printed["thread_id"].as_str().map(str::to_owned);
        }
        (Some("item.completed"), Some("agent_message")) => {
            said(&item["text"], folder).into_iter().for_each(tell);
        }
        (Some("item.started"), Some("command_execution")) => {
            did(Deed::Command, &item["command"], "")
                .into_iter()
                .for_each(tell);
        }
        (Some("item.completed"), Some("file_change")) => {
            for change in item["changes"].as_array().into_iter().flatten() {
                did(Deed::Edit, &change["path"], folder)
                    .into_iter()
                    .for_each(&mut *tell);
            }
        }
        (Some("turn.completed"), _) => return Some(Ok(())),
        // `error` comes before `turn.failed`, and alone when the turn couldn't start.
        (Some("turn.failed" | "error"), _) => {
            let why = text(&printed["error"]["message"]).or(text(&printed["message"]));
            return Some(Err(why.unwrap_or("Codex failed").to_owned()));
        }
        _ => {}
    }
    None
}

fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn said(words: &Value, folder: &str) -> Option<Event> {
    // An agent may link to a file by its whole path, which is the machine's own business.
    let text = text(words)?.replace(folder, "");
    Some(Event::Said { text })
}

fn did(deed: Deed, what: &Value, folder: &str) -> Option<Event> {
    let what = text(what)?;
    // A file is shown as it is in the folder.
    let what = what.strip_prefix(folder).unwrap_or(what);
    // Codex runs a command through a shell of its own, which the chat needn't show.
    let shell = ["/bin/zsh -lc ", "/bin/bash -lc ", "/bin/sh -lc "];
    let bare = shell.iter().find_map(|shell| what.strip_prefix(shell));
    let what = bare.map_or(what, |bare| bare.trim_matches(['\'', '"']));
    Some(Event::Did {
        deed,
        what: what.to_owned(),
    })
}
