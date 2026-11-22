//! Chats: each has a folder here, with a copy of its repository and what its turns told.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use core_link::{Agent, Changed, Event, FromCore, Repository};
use tokio::process::Command;
use tokio::sync::Notify;

use crate::home;
use crate::link::Shared;

/// A chat as the core knows it while it runs.
#[derive(Default)]
pub struct Chat {
    /// How many events its file holds.
    told: u64,
    /// Told to stop its turn, while one is running.
    turn: Option<Arc<Notify>>,
}

/// The chat's folder. Its name comes from the server, so it may only be an id.
fn folder(chat: &str) -> Option<PathBuf> {
    let id = !chat.is_empty() && chat.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    id.then(|| home().join("chats").join(chat))
}

fn numbered(chat: &str, number: u64, event: Event) -> FromCore {
    let chat = chat.to_owned();
    FromCore::Event {
        chat,
        number,
        event,
    }
}

/// Writes an event to the chat's file, so that it outlives the connection, and sends it.
fn tell(work: &Shared, chat: &str, event: Event) {
    let mut work = work.lock().unwrap();
    if let Some(folder) = folder(chat) {
        std::fs::create_dir_all(&folder).ok();
        let mut file = std::fs::OpenOptions::new();
        if let Ok(mut file) = file
            .create(true)
            .append(true)
            .open(folder.join("events.jsonl"))
        {
            let line = serde_json::to_string(&event).expect("an event is JSON");
            writeln!(file, "{line}").ok();
        }
    }
    let known = work.chats.entry(chat.to_owned()).or_default();
    known.told += 1;
    let number = known.told;
    if let Some(out) = &work.out {
        out.send(numbered(chat, number, event)).ok();
    }
}

/// Runs a chat's turn, or tells again what the server hasn't heard of the one it asks about.
pub fn turn(
    work: &Shared,
    chat: String,
    agent: Agent,
    session: Option<String>,
    prompt: String,
    repository: Repository,
    heard: u64,
) {
    let Some(folder) = folder(&chat) else { return };
    let file = std::fs::read_to_string(folder.join("events.jsonl")).unwrap_or_default();
    let lines = file.lines();
    let mut told: Vec<Event> = lines
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let running = {
        let mut work = work.lock().unwrap();
        let known = work.chats.entry(chat.clone()).or_default();
        known.told = told.len() as u64;
        known.turn.is_some()
    };
    let ended = matches!(
        told.last(),
        None | Some(Event::Done { .. } | Event::Failed { .. })
    );
    if !running && !ended {
        // The program went with the core that was running it.
        let why = "weacon-core was restarted during the turn".to_owned();
        tell(work, &chat, Event::Failed { why: why.clone() });
        told.push(Event::Failed { why });
    }
    if running || told.len() as u64 > heard {
        let work = work.lock().unwrap();
        let Some(out) = &work.out else { return };
        for (at, event) in told.into_iter().enumerate().skip(heard as usize) {
            out.send(numbered(&chat, at as u64 + 1, event)).ok();
        }
        return;
    }
    let stop = Arc::new(Notify::new());
    if let Some(chat) = work.lock().unwrap().chats.get_mut(&chat) {
        chat.turn = Some(stop.clone());
    }
    tokio::spawn({
        let work = work.clone();
        async move {
            let copy = folder.join("repository");
            let ended = match fetched(&copy, &repository).await {
                Ok(()) => {
                    let tell = |event| tell(&work, &chat, event);
                    let (session, stop) = (session.as_deref(), stop.notified());
                    let program = agent.program();
                    agent_run::turn(program, agent, &copy, session, &prompt, stop, tell).await
                }
                Err(why) => Err(format!("The repository couldn't be fetched: {why}")),
            };
            let ended = match ended {
                Ok(session) => Event::Done {
                    session,
                    changed: changed(&copy).await,
                },
                Err(why) => Event::Failed { why },
            };
            // Forgotten first, so that the next turn can start as soon as the server hears of the end.
            if let Some(chat) = work.lock().unwrap().chats.get_mut(&chat) {
                chat.turn = None;
            }
            tell(&work, &chat, ended);
        }
    });
}

/// Ends a chat's turn, and its program with it. The turn itself then tells that it was stopped.
pub fn stop(work: &Shared, chat: &str) {
    if let Some(Chat {
        turn: Some(stop), ..
    }) = work.lock().unwrap().chats.get(chat)
    {
        stop.notify_one();
    }
}

/// Runs git in a folder and returns what it prints, or what it complains of.
async fn git(folder: &Path, args: &[&str], header: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(folder).stdin(Stdio::null());
    command.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(header) = header {
        // Given in the environment, so that it is in no file and no list of running programs.
        command.env("GIT_CONFIG_COUNT", "1");
        command.env("GIT_CONFIG_KEY_0", "http.extraHeader");
        command.env("GIT_CONFIG_VALUE_0", header);
    }
    let output = command.kill_on_drop(true).output().await;
    let output = output.map_err(|error| format!("git could not be started: {error}"))?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        false => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
}

/// Makes the chat's copy of its repository, at the branch's newest commit, unless it is there.
async fn fetched(copy: &Path, repository: &Repository) -> Result<(), String> {
    if copy.join(".git").exists() {
        return Ok(());
    }
    let made = tokio::fs::create_dir_all(copy).await;
    made.map_err(|error| error.to_string())?;
    let basic = BASE64.encode(format!("x-access-token:{}", repository.token));
    let header = format!("Authorization: Basic {basic}");
    let fetch = [
        "fetch",
        "--quiet",
        "--depth",
        "1",
        &repository.url,
        &repository.branch,
    ];
    let branch = [
        "checkout",
        "--quiet",
        "-B",
        &repository.branch,
        "FETCH_HEAD",
    ];
    let fetched = async {
        git(copy, &["init", "--quiet"], None).await?;
        git(copy, &fetch, Some(&header)).await?;
        git(copy, &branch, None).await
    };
    if let Err(why) = fetched.await {
        // A half-made copy would pass for a whole one next time.
        tokio::fs::remove_dir_all(copy).await.ok();
        return Err(why);
    }
    Ok(())
}

/// The files that differ from the commit the copy started from, new ones included.
async fn changed(copy: &Path) -> Vec<Changed> {
    git(copy, &["add", "--all", "--intent-to-add"], None)
        .await
        .ok();
    let counted = git(copy, &["diff", "--numstat", "HEAD"], None).await;
    let counted = counted.unwrap_or_default();
    let files = counted.lines().filter_map(|line| {
        let mut parts = line.splitn(3, '\t');
        // A file that isn't text is counted as "-".
        let mut count = || parts.next().map(|count| count.parse().unwrap_or(0));
        let (added, removed) = (count()?, count()?);
        let path = parts.next()?.to_owned();
        Some(Changed {
            path,
            added,
            removed,
        })
    });
    files.collect()
}
