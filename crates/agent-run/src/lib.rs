//! Runs one turn of a coding agent's own program in a folder, and tells what it says and
//! does in words that are the same for every agent. What is one vendor's business, its
//! flags, its sandbox and what it prints, is in that vendor's file.

mod claude;
mod codex;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use core_link::{Agent, Deed, Event, Tools};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// The environment variables an agent's program gets: enough to be found and signed in.
/// Where each program finds the token for the server's tools, so that it is in no command line.
const TOKEN: &str = "WEACON_TOOLS";

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

/// How long a program that was told to stop has to end what it started.
const GRACE: Duration = Duration::from_secs(5);

/// One turn to run. The agent may change files in `folder` and run commands inside the
/// vendor's sandbox, which keeps writes to `folder` and `cache` and lets only `hosts` be
/// reached. Nothing asks a person, and none of the person's own setup is loaded.
pub struct Turn<'a> {
    /// `agent.program()`, or a test's stand-in for it.
    pub program: &'a str,
    pub agent: Agent,
    pub folder: &'a Path,
    /// The conversation to go on with.
    pub session: Option<&'a str>,
    pub prompt: &'a str,
    /// One that isn't an exact name is left out.
    pub hosts: &'a [String],
    /// For what commands download.
    pub cache: &'a Path,
    /// The server's tools for the chat, which the agent's program calls itself.
    pub tools: &'a Tools,
}

/// How a line that ends the turn says it ended: well, or with why not.
type Ended = Option<Result<(), String>>;

/// Runs a turn and calls `tell` with what the agent says and does, as it comes. Returns the
/// session to go on with next time, or why the turn failed. When `stop` ends, so does the
/// program, and the commands it was running with it.
pub async fn turn(
    turn: Turn<'_>,
    stop: impl Future<Output = ()>,
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
        tools,
    } = turn;
    // The sandboxes and the programs know a folder by its real path, which a link in it would hide.
    let real = |folder: &Path| std::fs::canonicalize(folder).unwrap_or(folder.to_owned());
    let (folder, cache) = (real(folder), real(cache));
    let hosts = hosts.iter().map(String::as_str);
    let hosts: Vec<&str> = hosts.filter(|name| core_link::host(name)).collect();
    let args = match agent {
        Agent::Claude => claude::args(session, &hosts, &cache.to_string_lossy(), &tools.url),
        Agent::Codex => codex::args(session, &hosts, &cache.to_string_lossy(), &tools.url),
    };
    let child = Command::new(program)
        .args(args)
        .current_dir(&folder)
        .env_clear()
        .envs(std::env::vars().filter(|(name, _)| KEPT.contains(&name.as_str())))
        .envs(CACHES.map(|(setting, tool)| (setting, cache.join(tool))))
        .env(TOKEN, &tools.token)
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

    let folder = format!("{}/", folder.to_string_lossy());
    let mut lines = BufReader::new(child.stdout.take().expect("stdout was piped")).lines();
    let mut session = session.map(str::to_owned);
    let mut ended = None;
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
            Agent::Claude => claude::read(&printed, &folder, &mut session, &mut tell),
            Agent::Codex => codex::read(&printed, &folder, &mut session, &mut tell),
        };
        ended = read.or(ended);
    }
    let left = child.wait().await.map_err(|error| error.to_string())?;
    match (ended, session) {
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
    Some(Event::Did {
        deed,
        what: what.to_owned(),
    })
}

/// The hosts a sandbox says it refused, in a command's `output`: each stands right after
/// `before`, and ends at the first of `after`.
fn wanted(output: &str, before: &str, after: char, tell: &mut dyn FnMut(Event)) {
    for refusal in output.split(before).skip(1) {
        let host = refusal.split(after).next().unwrap_or_default();
        if core_link::host(host) {
            let host = host.to_owned();
            tell(Event::Wants { host });
        }
    }
}
