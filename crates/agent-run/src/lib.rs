//! Runs one turn of a coding agent's own program in a folder, and tells what it says and
//! does in words that are the same for every agent. The vendors' flags, their sandboxes
//! and what they print are known here and nowhere else.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use core_link::{Agent, Deed, Event, host};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// The environment variables an agent's program gets: enough to be found and signed in.
pub const KEPT: [&str; 5] = ["PATH", "HOME", "USER", "CODEX_HOME", "CLAUDE_CONFIG_DIR"];

/// The tools that download packages, each by the setting that says where it keeps them and
/// the folder it gets in the chat's cache. Left alone, each writes in the person's home,
/// which the sandbox refuses.
const CACHES: [(&str, &str); 8] = [
    ("npm_config_cache", "npm"),
    ("YARN_CACHE_FOLDER", "yarn"),
    ("BUN_INSTALL_CACHE_DIR", "bun"),
    ("PIP_CACHE_DIR", "pip"),
    ("UV_CACHE_DIR", "uv"),
    ("COMPOSER_CACHE_DIR", "composer"),
    ("GOMODCACHE", "go/mod"),
    ("GOCACHE", "go/build"),
];

/// One turn to run: `program` is `agent.program()`, or a test's stand-in for it.
pub struct Turn<'a> {
    pub program: &'a str,
    pub agent: Agent,
    pub folder: &'a Path,
    /// The conversation to go on with.
    pub session: Option<&'a str>,
    pub prompt: &'a str,
    /// The hosts commands may reach. One that isn't an exact name is left out.
    pub hosts: &'a [String],
    /// A folder outside `folder` that commands may write in too, for what they download.
    pub cache: &'a Path,
}

/// How an agent's program is started for a turn. Each may change files in the folder and run
/// commands inside the vendor's sandbox, which keeps writes to the folder and `cache` and
/// lets only `hosts` be reached. None of the person's own setup is loaded.
fn args(agent: Agent, session: Option<&str>, hosts: &[&str], cache: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut add = |more: &[&str]| args.extend(more.iter().map(|arg| arg.to_string()));
    match agent {
        Agent::Claude => {
            // Claude Code's own sandbox for commands, with nothing let out of it.
            let sandbox = json!({"sandbox": {
                "enabled": true,
                "autoAllowBashIfSandboxed": true,
                "allowUnsandboxedCommands": false,
                "network": {"allowedDomains": hosts},
                "filesystem": {"allowWrite": [cache]},
            }});
            add(&[
                "--print",
                "--output-format",
                "stream-json",
                // Kept open, so that what it asks can be answered.
                "--input-format",
                "stream-json",
                "--verbose",
                "--safe-mode",
                "--strict-mcp-config",
                "--permission-mode",
                "acceptEdits",
                "--permission-prompts",
                "host",
                "--permission-prompt-tool",
                "stdio",
                // Every command is allowed, since every command is in the sandbox.
                "--allowedTools",
                "Bash",
                "--settings",
                &sandbox.to_string(),
            ]);
            if let Some(session) = session {
                add(&["--resume", session]);
            }
        }
        Agent::Codex => {
            add(&["exec"]);
            if let Some(session) = session {
                add(&["resume", session]);
            }
            let allowed: Vec<String> = hosts
                .iter()
                .map(|host| format!("\"{host}\"=\"allow\""))
                .collect();
            add(&[
                "--json",
                "--config",
                r#"sandbox_mode="workspace-write""#,
                // The network is there, and goes through a proxy that knows the hosts and names the one it refuses.
                "--config",
                "sandbox_workspace_write.network_access=true",
                "--config",
                "features.network_proxy.enabled=true",
                "--config",
                &format!("features.network_proxy.domains={{{}}}", allowed.join(",")),
                "--config",
                &format!("sandbox_workspace_write.writable_roots=[{}]", json!(cache)),
                "--ignore-user-config",
                "--ignore-rules",
                "--skip-git-repo-check",
                "-",
            ]);
        }
    }
    args
}

/// How long a program that was told to stop has to end what it started.
const GRACE: Duration = Duration::from_secs(5);

/// Runs a turn and calls `tell` with what the agent says and does, as it comes. Returns the
/// session to go on with next time, or why the turn failed. When `stop` ends, so does the
/// program, and the commands it was running with it. `answers` brings whether a host that
/// was asked about is allowed, by the id it was asked under.
pub async fn turn(
    turn: Turn<'_>,
    stop: impl Future<Output = ()>,
    answers: &mut mpsc::UnboundedReceiver<(String, bool)>,
    mut tell: impl FnMut(Event),
) -> Result<String, String> {
    let Turn {
        program,
        agent,
        folder,
        session,
        prompt,
        hosts,
        cache,
    } = turn;
    // The sandboxes know a folder by its real path, which a link in it would hide.
    let real = |folder: &Path| std::fs::canonicalize(folder).unwrap_or(folder.to_owned());
    let cache = real(cache);
    let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let hosts: Vec<&str> = hosts.into_iter().filter(|name| host(name)).collect();
    let child = Command::new(program)
        .args(args(agent, session, &hosts, &cache.to_string_lossy()))
        .current_dir(folder)
        .env_clear()
        .envs(std::env::vars().filter(|(name, _)| KEPT.contains(&name.as_str())))
        .envs(CACHES.map(|(setting, tool)| (setting, cache.join(tool))))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = child.map_err(|error| format!("{program} could not be started: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    // Written on the side, so a program that prints before it has read everything can't
    // block us. Its input closes when `write` is dropped.
    let (write, mut written) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(text) = written.recv().await {
            if stdin.write_all(text.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let line = |message: Value| format!("{message}\n");
    let mut write = match agent {
        Agent::Claude => {
            let message = json!({"role": "user", "content": prompt});
            write
                .send(line(json!({"type": "user", "message": message})))
                .ok();
            Some(write)
        }
        // Codex reads its prompt to the end of its input.
        Agent::Codex => {
            write.send(prompt.to_owned()).ok();
            drop(write);
            None
        }
    };
    // What Claude Code asked about and is waiting to hear, by the id it asked under.
    let mut asked: HashMap<String, Value> = HashMap::new();
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let complaint = tokio::spawn(async move {
        let mut complaint = String::new();
        stderr.read_to_string(&mut complaint).await.ok();
        complaint
    });

    let folder = format!("{}/", real(folder).to_string_lossy());
    let mut lines = BufReader::new(child.stdout.take().expect("stdout was piped")).lines();
    let mut session = session.map(str::to_owned);
    let mut ended = None;
    let session = &mut session;
    let mut stop = std::pin::pin!(stop);
    loop {
        let printed = tokio::select! {
            printed = lines.next_line() => printed,
            Some((ask, allowed)) = answers.recv() => {
                if let (Some(input), Some(write)) = (asked.remove(&ask), &write) {
                    let answer = match allowed {
                        true => json!({"behavior": "allow", "updatedInput": input}),
                        false => json!({"behavior": "deny", "message": REFUSED}),
                    };
                    write.send(line(answered(&ask, answer))).ok();
                }
                continue;
            }
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
        let Ok(Some(printed)) = printed else { break };
        let Ok(printed) = serde_json::from_str::<Value>(&printed) else {
            continue;
        };
        if let (Some(ask), Some(write)) = (printed["request_id"].as_str(), &write)
            && printed["type"] == "control_request"
        {
            let request = &printed["request"];
            let wanted = text(&request["input"]["host"]).filter(|name| host(name));
            match wanted.filter(|_| request["tool_name"] == "SandboxNetworkAccess") {
                Some(host) => {
                    asked.insert(ask.to_owned(), request["input"].clone());
                    tell(Event::Wants {
                        host: host.to_owned(),
                        ask: Some(ask.to_owned()),
                    });
                }
                // Anything else it would ask a person is refused, as nothing but a host can be allowed.
                None => {
                    let refused =
                        json!({"behavior": "deny", "message": "This isn't allowed in this chat."});
                    write.send(line(answered(ask, refused))).ok();
                }
            }
            continue;
        }
        let read = match agent {
            Agent::Claude => claude(&printed, &folder, session, &mut tell),
            Agent::Codex => codex(&printed, &folder, session, &mut tell),
        };
        if read.is_some() {
            // Claude Code goes when its input ends.
            write = None;
        }
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

/// What a command is told when its host was refused.
const REFUSED: &str = "The person didn't allow this host.";

/// Claude Code's own words for the answer to what it asked under an id.
fn answered(ask: &str, answer: Value) -> Value {
    let response = json!({"subtype": "success", "request_id": ask, "response": answer});
    json!({"type": "control_response", "response": response})
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
        (Some("item.completed"), Some("command_execution")) => {
            // The sandbox's proxy says in the command's output which host it refused.
            let output = item["aggregated_output"].as_str().unwrap_or_default();
            for refusal in output.split("Network access to \"").skip(1) {
                let name = refusal.split('"').next().unwrap_or_default();
                if refusal.contains("not on the allowlist") && host(name) {
                    let (host, ask) = (name.to_owned(), None);
                    tell(Event::Wants { host, ask });
                }
            }
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
