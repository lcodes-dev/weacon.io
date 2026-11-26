//! The team's own agent, called in a group as `@weacon`. It runs here, on the team's API
//! key, and answers from the tasks and docs of the group's project with the tools a chat's
//! agent has. It keeps nothing between calls.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::chats::Caller;
use crate::error::{ApiError, Result};
use crate::{AppState, anthropic, db, live, tools};

/// How often it may ask for its tools in one call.
const ROUNDS: usize = 10;

const SYSTEM: &str = "You are Weacon, the agent of a team, called in one of its groups about one \
    of its projects. You know the project by its tasks and its docs, which you read and search \
    with your tools; the docs say which files they were written from. You can't read the code \
    itself or run anything, and you say so when that is what an answer needs. You remember \
    nothing between calls: what was said before is among the group's messages. Answer briefly, \
    in Markdown.";

#[derive(Clone, Serialize, Deserialize)]
pub struct AnswerAsWeacon {
    pub group_id: String,
    /// What was written in the group, as it is told to the agent.
    pub asked: String,
    pub caller: Caller,
}

impl AnswerAsWeacon {
    /// One call at a time in a group, and while it runs the group shows Weacon working.
    pub fn key(group_id: &str) -> String {
        format!("weacon:{group_id}")
    }

    pub async fn handle(self, state: &AppState) -> Result<()> {
        let team_id = &self.caller.team_id;
        let saved = db::settings::api_key(&state.pool, &state.server_key, team_id).await?;
        let (key, model) = saved.ok_or_else(|| ApiError::invalid("The team's API key is gone"))?;
        let model = anthropic::model(model.as_deref());
        let answer = self.answer(state, &key, model).await?;
        self.post(state, &answer).await;
        Ok(())
    }

    pub async fn failed(self, state: &AppState, why: String) {
        self.post(state, &format!("It couldn't answer: {why}"))
            .await;
    }

    /// Asks the model, runs each tool it asks for and tells it the result, until it has its answer.
    async fn answer(&self, state: &AppState, key: &str, model: &str) -> Result<String> {
        // The same tools, as Anthropic names their parts.
        let tool = |tool: &Value| {
            let (name, does, takes) = (&tool["name"], &tool["description"], &tool["inputSchema"]);
            json!({"name": name, "description": does, "input_schema": takes})
        };
        let tools: Vec<Value> = tools::list()
            .as_array()
            .into_iter()
            .flatten()
            .map(tool)
            .collect();
        let mut messages = vec![json!({"role": "user", "content": self.asked})];
        for _ in 0..ROUNDS {
            let body = json!({
                "model": model, "max_tokens": 16000, "system": SYSTEM, "tools": tools,
                "messages": messages,
            });
            let answer = anthropic::send(state, key, &body).await?;
            match answer["stop_reason"].as_str() {
                Some("end_turn") => {
                    let text = anthropic::text(&answer);
                    if text.trim().is_empty() {
                        return Ok("It had nothing to say.".into());
                    }
                    return Ok(text);
                }
                Some("tool_use") => {}
                _ => return Err(anthropic::stopped(&answer)),
            }
            let blocks = answer["content"].as_array().cloned().unwrap_or_default();
            let mut results = Vec::new();
            for block in blocks.iter().filter(|block| block["type"] == "tool_use") {
                let name = block["name"].as_str().unwrap_or_default();
                let done = tools::call(state, &self.caller, name, &block["input"]).await;
                let failed = done.is_err();
                let text = done.unwrap_or_else(|error| error.message);
                results.push(json!({
                    "type": "tool_result", "tool_use_id": block["id"], "content": text,
                    "is_error": failed,
                }));
            }
            messages.push(json!({"role": "assistant", "content": blocks}));
            messages.push(json!({"role": "user", "content": results}));
        }
        Err(ApiError::invalid(format!(
            "It asked for its tools more than {ROUNDS} times"
        )))
    }

    /// The group may be gone by now, and then nobody is told.
    async fn post(&self, state: &AppState, text: &str) {
        let (group, user) = (&self.group_id, &self.caller.user_id);
        if db::groups::post_as_weacon(&state.pool, group, user, text)
            .await
            .is_ok()
        {
            live::group(state, group).await;
        }
    }
}
