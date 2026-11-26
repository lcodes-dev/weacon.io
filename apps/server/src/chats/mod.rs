pub mod db;
pub mod http;
pub mod tools;

use core_link::{Agent, Event, Repository, ToCore, Tools};
use serde::{Deserialize, Serialize};

use crate::chats::db::Running;
use crate::error::{ApiError, Result};
use crate::jobs::Job;
use crate::{AppState, jobs, live, projects};

pub async fn start_turn(state: &AppState, machine_id: &str, chat_id: &str) -> Result<()> {
    let job = StartTurn {
        machine_id: machine_id.to_owned(),
        chat_id: chat_id.to_owned(),
    };
    jobs::perform_later(state, Job::StartTurn(job)).await?;
    Ok(())
}

pub async fn resume_turns(state: &AppState, machine_id: &str) {
    let running = db::running(&state.pool, Some(machine_id)).await;
    for chat in running.unwrap_or_default() {
        send_turn(state, machine_id, chat).await;
    }
}

pub async fn end_orphaned_turns(state: &AppState) {
    let running = db::running(&state.pool, None).await;
    for chat in running.unwrap_or_default() {
        fail(state, &chat.id, "The machine was removed").await;
    }
}

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
    let token = projects::github::token(
        state,
        &chat.team_id,
        Some(repo),
        projects::github::reading(),
    )
    .await?;
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
    db::fail(&state.pool, chat_id, why).await.ok();
    live::chat(state, chat_id).await;
}

pub async fn hear(state: &AppState, machine_id: &str, chat_id: &str, number: u64, event: Event) {
    let heard = db::hear(&state.pool, machine_id, chat_id, number as i64, &event);
    heard.await.ok();
    live::chat(state, chat_id).await;
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StartTurn {
    pub machine_id: String,
    pub chat_id: String,
}

impl StartTurn {
    pub async fn perform(self, state: &AppState) -> Result<()> {
        live::chat(state, &self.chat_id).await;
        let running = db::running(&state.pool, Some(&self.machine_id)).await?;
        if let Some(chat) = running.into_iter().find(|chat| chat.id == self.chat_id) {
            send_turn(state, &self.machine_id, chat).await;
        }
        Ok(())
    }
}
