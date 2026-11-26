mod claude;
mod codex;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use core_link::{Agent, Deed, Event, Tools};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const TOKEN: &str = "WEACON_TOOLS";

pub const KEPT: [&str; 5] = ["PATH", "HOME", "USER", "CODEX_HOME", "CLAUDE_CONFIG_DIR"];

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

const GRACE: Duration = Duration::from_secs(5);

pub struct Turn<'a> {
    pub program: &'a str,
    pub agent: Agent,
    pub folder: &'a Path,
    pub session: Option<&'a str>,
    pub prompt: &'a str,
    pub hosts: &'a [String],
    pub cache: &'a Path,
    pub tools: &'a Tools,
}

type Ended = Option<Result<(), String>>;

pub async fn turn(
    turn: Turn<'_>,
    stop: impl Future<Output = ()>,
    mut tell: impl FnMut(Event),
) -> Result<String, String> {
    let Turn { program, agent, folder, session, prompt, hosts, cache, tools } = turn;
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
                    Command::new("kill").args(["-INT", &id.to_string()]).status().await.ok();
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
            let tail = complaint.trim().char_indices().rev().nth(399);
            let tail = &complaint.trim()[tail.map_or(0, |(at, _)| at)..];
            Err(format!("{program} stopped before the turn ended ({left}): {tail}"))
        }
    }
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|text| !text.is_empty())
}

fn said(words: &Value, folder: &str) -> Option<Event> {
    let text = text(words)?.replace(folder, "");
    Some(Event::Said { text })
}

fn did(deed: Deed, what: &Value, folder: &str) -> Option<Event> {
    let what = text(what)?;
    let what = what.strip_prefix(folder).unwrap_or(what);
    Some(Event::Did { deed, what: what.to_owned() })
}

fn wanted(output: &str, before: &str, after: char, tell: &mut dyn FnMut(Event)) {
    for refusal in output.split(before).skip(1) {
        let host = refusal.split(after).next().unwrap_or_default();
        if core_link::host(host) {
            let host = host.to_owned();
            tell(Event::Wants { host });
        }
    }
}
