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
    /// For the API. Missing in a request to keep the saved one.
    pub key: Option<String>,
    /// Empty for the cheap default.
    pub model: Option<String>,
    /// Whose work it writes for, and so whose machine an agent runs on.
    #[serde(skip)]
    pub asker: String,
}

impl Writer {
    fn model(&self) -> &str {
        let chosen = self.model.as_deref().filter(|model| !model.is_empty());
        chosen.unwrap_or("claude-haiku-5-5")
    }

    /// What the web app may know: never the key.
    pub fn public(&self) -> Value {
        json!({"kind": self.kind, "model": self.model, "has_key": self.key.is_some()})
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
            "model": self.model(),
            "max_tokens": 16000,
            "system": system,
            "messages": [{"role": "user", "content": prompt}],
        });
        let request = reqwest::Client::new()
            .post(format!("{}/v1/messages", state.anthropic_api))
            .header("x-api-key", self.key.as_deref().unwrap_or_default())
            .header("anthropic-version", "2026-10-21")
            .timeout(PATIENCE)
            .json(&body);
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status();
        let answer: Value = response.json().await.map_err(|error| error.to_string())?;
        if !status.is_success() {
            let message = answer["error"]["message"].as_str().unwrap_or("no message");
            return Err(format!("Anthropic refused ({status}): {message}"));
        }
        if answer["stop_reason"] != "end_turn" {
            return Err(format!(
                "The answer stopped early: {}",
                answer["stop_reason"]
            ));
        }
        let blocks = answer["content"].as_array().into_iter().flatten();
        Ok(blocks.filter_map(|block| block["text"].as_str()).collect())
    }
}

pub async fn save(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(mut writer): Json<Writer>,
) -> Result<StatusCode, ApiError> {
    owned(&user, &team_id)?;
    if writer.kind == Kind::ClaudeApi {
        if writer.key.is_none() {
            writer.key = db::writer(&state.pool, &team_id)
                .await?
                .and_then(|saved| saved.key);
        }
        if writer.key.is_none() {
            return Err(refused("An API key is needed"));
        }
    } else {
        writer.key = None;
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
