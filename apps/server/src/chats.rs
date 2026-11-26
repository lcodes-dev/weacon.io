//! A chat's turns: each is sent to the core of the chat's machine, which runs the agent and
//! tells what happened, event by event.

use core_link::{Agent, Event, Repository, ToCore, Tools};

use crate::db::chats::Running;
use crate::error::{ApiError, Result};
use crate::jobs::{self, Job, StartTurn};
use crate::{AppState, db, github, live};

/// Has the chat's machine run the turn that was just asked for, once the request is answered.
pub async fn start_turn(state: &AppState, machine_id: &str, chat_id: &str) -> Result<()> {
    let job = StartTurn {
        machine_id: machine_id.to_owned(),
        chat_id: chat_id.to_owned(),
    };
    jobs::dispatch(state, Job::StartTurn(job)).await?;
    Ok(())
}

/// Tells a machine that just connected of the turns it should be running: those it never
/// heard of start, and those it was running say what the server missed.
pub async fn resume_turns(state: &AppState, machine_id: &str) {
    let running = db::chats::running(&state.pool, Some(machine_id)).await;
    for chat in running.unwrap_or_default() {
        send_turn(state, machine_id, chat).await;
    }
}

/// Ends the turns whose machine was removed, which nothing else would end.
pub async fn end_orphaned_turns(state: &AppState) {
    let running = db::chats::running(&state.pool, None).await;
    for chat in running.unwrap_or_default() {
        fail(state, &chat.id, "The machine was removed").await;
    }
}

/// Sends a turn to its machine, if that is connected; one that is away is sent it when it connects.
pub async fn send_turn(state: &AppState, machine_id: &str, chat: Running) {
    if !state.cores.connected(machine_id) {
        return;
    }
    let chat_id = chat.id.clone();
    match turn(state, chat).await {
        Ok(turn) => {
            state.cores.send(machine_id, ToCore::Turn(turn));
        }
        Err(error) => fail(state, &chat_id, &error.message).await,
    }
}

async fn turn(state: &AppState, chat: Running) -> Result<core_link::Turn> {
    let no_repository = || ApiError::invalid("The project has no repository any more");
    let repo = chat.repo.as_deref().ok_or_else(no_repository)?;
    let agent = Agent::named(&chat.agent).ok_or_else(ApiError::not_found)?;
    // The machine fetches with a token for this repository alone, which GitHub ends after an hour.
    let token = github::token(state, &chat.team_id, Some(repo), github::reading()).await?;
    Ok(core_link::Turn {
        repository: repository(repo, chat.branch, token),
        chat: chat.id,
        agent,
        session: chat.session,
        prompt: chat.asked,
        heard: chat.heard as u64,
        hosts: chat.hosts.0,
        tools: Tools {
            url: format!("{}/api/tools", state.url),
            token: chat.tools_token.unwrap_or_default(),
        },
    })
}

pub fn repository(repo: &str, branch: String, token: String) -> Repository {
    Repository {
        url: format!("https://github.com/{repo}.git"),
        branch,
        token,
    }
}

async fn fail(state: &AppState, chat_id: &str, why: &str) {
    db::chats::fail(&state.pool, chat_id, why).await.ok();
    live::chat(state, chat_id).await;
}

/// Takes in what a machine tells of a chat of its own.
pub async fn hear(state: &AppState, machine_id: &str, chat_id: &str, number: u64, event: Event) {
    let heard = db::chats::hear(&state.pool, machine_id, chat_id, number as i64, &event);
    heard.await.ok();
    live::chat(state, chat_id).await;
}
