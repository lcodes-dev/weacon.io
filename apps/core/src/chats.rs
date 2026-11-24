//! Chats: each has a folder here, with a copy of its repository and what its turns told.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use core_link::{Event, FromCore, Repository, Turn};
use tokio::sync::{Notify, mpsc};

use crate::home;
use crate::link::Shared;

/// A chat as the core knows it while it runs.
#[derive(Default)]
pub struct Chat {
    /// How many events its file holds.
    told: u64,
    /// Its turn, while one is running.
    turn: Option<Running>,
}

struct Running {
    /// Told to stop the turn.
    stop: Arc<Notify>,
    /// Takes whether a host the agent asked about is allowed, by the id it asked under.
    answers: mpsc::UnboundedSender<(String, bool)>,
}

/// The chat's folder. Its name comes from the server, so it may only be an id.
fn folder(chat: &str) -> Option<PathBuf> {
    let id = !chat.is_empty() && chat.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    id.then(|| home().join("chats").join(chat))
}

/// Any other kind of address could name a folder of this machine, or a program to run.
fn safe(repository: &Repository) -> bool {
    repository.url.starts_with("https://")
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
pub fn turn(work: &Shared, turn: Turn) {
    let Turn {
        chat,
        agent,
        session,
        prompt,
        repository,
        heard,
        hosts,
    } = turn;
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
    let (answer, mut answers) = mpsc::unbounded_channel();
    if let Some(chat) = work.lock().unwrap().chats.get_mut(&chat) {
        chat.turn = Some(Running {
            stop: stop.clone(),
            answers: answer,
        });
    }
    tokio::spawn({
        let work = work.clone();
        async move {
            let fetched = match safe(&repository) {
                true => chat_copy::fetch(&folder, &repository).await,
                false => Err("its address isn't an https one".into()),
            };
            let ended = match fetched {
                Ok(()) => {
                    let tell = |event| tell(&work, &chat, event);
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
                    };
                    agent_run::turn(asked, stop.notified(), &mut answers, tell).await
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
        turn: Some(running),
        ..
    }) = work.lock().unwrap().chats.get(chat)
    {
        running.stop.notify_one();
    }
}

/// Hands the chat's running turn whether a host its agent asked about is allowed.
pub fn answer(work: &Shared, chat: &str, ask: String, allowed: bool) {
    if let Some(Chat {
        turn: Some(running),
        ..
    }) = work.lock().unwrap().chats.get(chat)
    {
        running.answers.send((ask, allowed)).ok();
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
