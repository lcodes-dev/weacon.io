use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use core_link::Agent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{self, User};
use crate::teams::{owned, refused};
use crate::{ApiError, AppState, machines};

/// A page can take minutes to write.
const PATIENCE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Kind {
    /// The Anthropic API, on the server, paid by the team's key.
    ClaudeApi,
    /// Claude Code on a machine of whoever starts the work, on their own sign-in.
    ClaudeCode,
    /// Codex, likewise.
    Codex,
}

/// Who writes a team's docs.
#[derive(Deserialize)]
pub struct Writer {
    pub kind: Kind,
    /// The team's API key, if it has one.
    #[serde(skip)]
    pub key: Option<String>,
    /// Empty for the cheap default.
    pub model: Option<String>,
    /// Whose work it writes for, and so whose machine an agent runs on.
    #[serde(skip)]
    pub asker: String,
}

impl Writer {
    /// What the web app may know: never the key.
    pub fn public(&self) -> Value {
        json!({"kind": self.kind, "model": self.model})
    }

    /// Sends the prompt and returns the answer's text, or why there is none.
    pub async fn ask(
        &self,
        state: &AppState,
        system: &str,
        prompt: &str,
    ) -> Result<String, String> {
        // A machine's core knows its agent's cheap default; the API is told ours.
        let model = self.model.as_deref().unwrap_or_default();
        let agent = match self.kind {
            Kind::ClaudeApi => return self.ask_api(state, system, prompt).await,
            Kind::ClaudeCode => Agent::Claude,
            Kind::Codex => Agent::Codex,
        };
        machines::ask(state, &self.asker, agent, model, system, prompt).await
    }

    async fn ask_api(
        &self,
        state: &AppState,
        system: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let body = json!({
            "model": model(self.model.as_deref()),
            "max_tokens": 16000,
            "system": system,
            "messages": [{"role": "user", "content": prompt}],
        });
        let key = self.key.as_deref().unwrap_or_default();
        let answer = anthropic(state, key, &body).await?;
        if answer["stop_reason"] != "end_turn" {
            return Err(stopped(&answer));
        }
        Ok(text(&answer))
    }
}

/// The model a team chose, or the cheap default.
pub fn model(chosen: Option<&str>) -> &str {
    chosen
        .filter(|model| !model.is_empty())
        .unwrap_or("claude-haiku-5-5")
}

/// Sends one request to the Anthropic API with a team's key, and gives its answer.
pub async fn anthropic(state: &AppState, key: &str, body: &Value) -> Result<Value, String> {
    let request = reqwest::Client::new()
        .post(format!("{}/v1/messages", state.anthropic_api))
        .header("x-api-key", key)
        .header("anthropic-version", "2026-10-21")
        .timeout(PATIENCE)
        .json(body);
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let answer: Value = response.json().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        let message = answer["error"]["message"].as_str().unwrap_or("no message");
        return Err(format!("Anthropic refused ({status}): {message}"));
    }
    Ok(answer)
}

/// The words in an answer.
pub fn text(answer: &Value) -> String {
    let blocks = answer["content"].as_array().into_iter().flatten();
    blocks.filter_map(|block| block["text"].as_str()).collect()
}

pub fn stopped(answer: &Value) -> String {
    format!("The answer stopped early: {}", answer["stop_reason"])
}

pub async fn save(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(writer): Json<Writer>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    let key = db::api_key(&state.pool, &state.server_key, &team_id).await?;
    if writer.kind == Kind::ClaudeApi && key.is_none() {
        return Err(refused("Save the team's API key first"));
    }
    db::save_writer(&state.pool, &team_id, Some(&writer)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn forget(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    db::save_writer(&state.pool, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Key {
    key: String,
}

/// Keeps the team's API key, which pays for its own agent and, if chosen, for its docs.
pub async fn save_key(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(Key { key }): Json<Key>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(refused("An API key is needed"));
    }
    db::save_api_key(&state.pool, &state.server_key, &team_id, Some(key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn forget_key(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    let writer = db::writer(&state.pool, &state.server_key, &team_id).await?;
    if writer.is_some_and(|writer| writer.kind == Kind::ClaudeApi) {
        return Err(refused(
            "The team's docs are written with this key. Choose another writer first",
        ));
    }
    db::save_api_key(&state.pool, &state.server_key, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
