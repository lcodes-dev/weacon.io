use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use core_link::{Event, FromCore, Repository, Turn};
use tokio::sync::Notify;

use crate::home;
use crate::link::{Shared, Work};

fn folder(chat: &str) -> Option<PathBuf> {
    let id = !chat.is_empty() && chat.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    id.then(|| home().join("chats").join(chat))
}

fn safe(repository: &Repository) -> bool {
    repository.url.starts_with("https://")
}

fn told(folder: &Path) -> Vec<String> {
    let file = std::fs::read_to_string(folder.join("events.jsonl")).unwrap_or_default();
    file.lines().map(str::to_owned).collect()
}

fn write(folder: &Path, event: &Event) -> Option<String> {
    let line = serde_json::to_string(event).expect("an event is JSON");
    std::fs::create_dir_all(folder).ok()?;
    let events = folder.join("events.jsonl");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(events).ok()?;
    writeln!(file, "{line}").ok()?;
    Some(line)
}

fn send(work: &Work, chat: &str, told: &[String], heard: usize) {
    let Some(out) = &work.out else { return };
    for (at, line) in told.iter().enumerate().skip(heard) {
        if let Ok(event) = serde_json::from_str(line) {
            let (chat, number) = (chat.to_owned(), at as u64 + 1);
            out.send(FromCore::Event { chat, number, event }).ok();
        }
    }
}

fn tell(work: &Shared, folder: &Path, chat: &str, event: Event) {
    let work = work.lock().unwrap();
    if write(folder, &event).is_some() {
        let told = told(folder);
        send(&work, chat, &told, told.len() - 1);
    }
}

pub fn turn(work: &Shared, turn: Turn) {
    let Some(folder) = folder(&turn.chat) else {
        return;
    };
    if !caught_up(work, &folder, &turn.chat, turn.heard as usize) {
        run(work, folder, turn);
    }
}

fn caught_up(work: &Shared, folder: &Path, chat: &str, heard: usize) -> bool {
    let work = work.lock().unwrap();
    let running = work.chats.contains_key(chat);
    let last = told(folder).pop().and_then(|line| serde_json::from_str(&line).ok());
    let unfinished = matches!(last, Some(Event::Said { .. } | Event::Did { .. } | Event::Wants { .. }));
    if !running && unfinished {
        let why = "weacon-core was restarted during the turn".to_owned();
        write(folder, &Event::Failed { why });
    }
    let told = told(folder);
    send(&work, chat, &told, heard);
    running || told.len() > heard
}

fn run(work: &Shared, folder: PathBuf, turn: Turn) {
    let Turn { chat, agent, session, prompt, repository, hosts, tools, .. } = turn;
    let stop = Arc::new(Notify::new());
    let running = stop.clone();
    work.lock().unwrap().chats.insert(chat.clone(), running);
    let work = work.clone();
    tokio::spawn(async move {
        let fetched = if safe(&repository) {
            chat_copy::fetch(&folder, &repository).await
        } else {
            Err("its address isn't an https one".into())
        };
        let ended = match fetched {
            Ok(()) => {
                let cache = chat_copy::cache(&folder);
                std::fs::create_dir_all(&cache).ok();
                let asked = agent_run::Turn {
                    program: agent.program(),
                    agent,
                    folder: &chat_copy::folder(&folder),
                    session: session.as_deref(),
                    prompt: &prompt,
                    hosts: &hosts,
                    cache: &cache,
                    tools: &tools,
                };
                let tell = |event| tell(&work, &folder, &chat, event);
                agent_run::turn(asked, stop.notified(), tell).await
            }
            Err(why) => Err(format!("The repository couldn't be fetched: {why}")),
        };
        let ended = match ended {
            Ok(session) => {
                let (changed, diff) = chat_copy::changes(&folder).await;
                Event::Done { session, changed, diff }
            }
            Err(why) => Event::Failed { why },
        };
        work.lock().unwrap().chats.remove(&chat);
        tell(&work, &folder, &chat, ended);
    });
}

pub fn stop(work: &Shared, chat: &str) {
    if let Some(stop) = work.lock().unwrap().chats.get(chat) {
        stop.notify_one();
    }
}

pub fn forget(chat: &str) -> Result<String, String> {
    let folder = folder(chat).ok_or("This isn't a chat's folder")?;
    match std::fs::remove_dir_all(folder) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("The chat's folder couldn't be removed: {error}"))
        }
        _ => Ok(String::new()),
    }
}

pub async fn push(
    chat: &str,
    repository: Repository,
    to: &str,
    message: &str,
    author: (&str, &str),
) -> Result<String, String> {
    let folder = folder(chat).filter(|_| safe(&repository));
    let folder = folder.ok_or("This isn't a chat's repository")?;
    chat_copy::push(&folder, &repository, to, message, author).await
}
