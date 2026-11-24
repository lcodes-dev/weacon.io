//! Chats: each has a folder here, with a copy of its repository and what its turns told.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use core_link::{Event, FromCore, Repository, Turn};
use tokio::sync::Notify;

use crate::home;
use crate::link::{Shared, Work};

/// The chat's folder. Its name comes from the server, so it may only be an id.
fn folder(chat: &str) -> Option<PathBuf> {
    let id = !chat.is_empty() && chat.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    id.then(|| home().join("chats").join(chat))
}

/// Any other kind of address could name a folder of this machine, or a program to run.
fn safe(repository: &Repository) -> bool {
    repository.url.starts_with("https://")
}

/// What the chat's turns told: an event a line, numbered by its line from 1. Each is
/// written here before it is sent, so that it outlives the connection.
fn told(folder: &Path) -> Vec<String> {
    let file = std::fs::read_to_string(folder.join("events.jsonl")).unwrap_or_default();
    file.lines().map(str::to_owned).collect()
}

/// Writes an event as the next line of the chat's file, and returns that line.
fn write(folder: &Path, event: &Event) -> Option<String> {
    let line = serde_json::to_string(event).expect("an event is JSON");
    std::fs::create_dir_all(folder).ok()?;
    let mut file = std::fs::OpenOptions::new();
    let file = file.create(true).append(true);
    let mut file = file.open(folder.join("events.jsonl")).ok()?;
    writeln!(file, "{line}").ok()?;
    Some(line)
}

/// Sends the server the chat's events from the line after `heard` on, if it is connected.
fn send(work: &Work, chat: &str, told: &[String], heard: usize) {
    let Some(out) = &work.out else { return };
    for (at, line) in told.iter().enumerate().skip(heard) {
        if let Ok(event) = serde_json::from_str(line) {
            let (chat, number) = (chat.to_owned(), at as u64 + 1);
            out.send(FromCore::Event {
                chat,
                number,
                event,
            })
            .ok();
        }
    }
}

/// Writes an event to the chat's file and sends it.
fn tell(work: &Shared, folder: &Path, chat: &str, event: Event) {
    // One at a time with whatever else sends, so that the server gets a chat's events in their order.
    let work = work.lock().unwrap();
    if write(folder, &event).is_some() {
        let told = told(folder);
        send(&work, chat, &told, told.len() - 1);
    }
}

/// Runs a chat's turn, unless the server only hasn't heard all of the last one.
pub fn turn(work: &Shared, turn: Turn) {
    let Some(folder) = folder(&turn.chat) else {
        return;
    };
    if !caught_up(work, &folder, &turn.chat, turn.heard as usize) {
        run(work, folder, turn);
    }
}

/// Tells the server what it hasn't heard of the chat, which includes that a turn was lost
/// when the core was restarted. False when it has heard everything and nothing runs, so
/// that the turn it asks for is a new one.
fn caught_up(work: &Shared, folder: &Path, chat: &str, heard: usize) -> bool {
    let work = work.lock().unwrap();
    let running = work.chats.contains_key(chat);
    let last = told(folder).pop();
    let last = last.and_then(|line| serde_json::from_str(&line).ok());
    let unfinished = matches!(
        last,
        Some(Event::Said { .. } | Event::Did { .. } | Event::Wants { .. })
    );
    if !running && unfinished {
        // The program went with the core that was running it.
        let why = "weacon-core was restarted during the turn".to_owned();
        write(folder, &Event::Failed { why });
    }
    let told = told(folder);
    send(&work, chat, &told, heard);
    running || told.len() > heard
}

fn run(work: &Shared, folder: PathBuf, turn: Turn) {
    let Turn {
        chat,
        agent,
        session,
        prompt,
        repository,
        hosts,
        tools,
        ..
    } = turn;
    let stop = Arc::new(Notify::new());
    let running = stop.clone();
    work.lock().unwrap().chats.insert(chat.clone(), running);
    let work = work.clone();
    tokio::spawn(async move {
        let fetched = match safe(&repository) {
            true => chat_copy::fetch(&folder, &repository).await,
            false => Err("its address isn't an https one".into()),
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
                Event::Done {
                    session,
                    changed,
                    diff,
                }
            }
            Err(why) => Event::Failed { why },
        };
        // Forgotten first, so that the next turn can start as soon as the server hears of the end.
        work.lock().unwrap().chats.remove(&chat);
        tell(&work, &folder, &chat, ended);
    });
}

/// Ends a chat's turn, and its program with it. The turn itself then tells that it was stopped.
pub fn stop(work: &Shared, chat: &str) {
    if let Some(stop) = work.lock().unwrap().chats.get(chat) {
        stop.notify_one();
    }
}

/// Commits and pushes what a chat's folder holds changed, and returns the commit.
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
