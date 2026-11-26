use core_link::Agent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::Result;
use crate::{AppState, anthropic, machines};

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Kind {
    ClaudeApi,
    ClaudeCode,
    Codex,
}

#[derive(Deserialize)]
pub struct Writer {
    pub kind: Kind,
    #[serde(skip)]
    pub key: Option<String>,
    pub model: Option<String>,
    #[serde(skip)]
    pub asker: String,
}

impl Writer {
    pub fn public(&self) -> Value {
        json!({"kind": self.kind, "model": self.model})
    }

    pub async fn ask(&self, state: &AppState, system: &str, prompt: &str) -> Result<String> {
        let model = self.model.as_deref().unwrap_or_default();
        let agent = match self.kind {
            Kind::ClaudeApi => return self.ask_api(state, system, prompt).await,
            Kind::ClaudeCode => Agent::Claude,
            Kind::Codex => Agent::Codex,
        };
        machines::ask(state, &self.asker, agent, model, system, prompt).await
    }

    async fn ask_api(&self, state: &AppState, system: &str, prompt: &str) -> Result<String> {
        let body = json!({
            "model": anthropic::model(self.model.as_deref()),
            "max_tokens": 16000,
            "system": system,
            "messages": [{"role": "user", "content": prompt}],
        });
        let key = self.key.as_deref().unwrap_or_default();
        let answer = anthropic::send(state, key, &body).await?;
        if answer["stop_reason"] != "end_turn" {
            return Err(anthropic::stopped(&answer));
        }
        Ok(anthropic::text(&answer))
    }
}
