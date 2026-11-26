use std::time::Duration;

use serde_json::Value;

use crate::AppState;
use crate::error::{ApiError, Result};

const PATIENCE: Duration = Duration::from_secs(15 * 60);

pub fn model(chosen: Option<&str>) -> &str {
    chosen
        .filter(|model| !model.is_empty())
        .unwrap_or("claude-haiku-5-5")
}

pub async fn send(state: &AppState, key: &str, body: &Value) -> Result<Value> {
    let request = reqwest::Client::new()
        .post(format!("{}/v1/messages", state.anthropic_api))
        .header("x-api-key", key)
        .header("anthropic-version", "2026-10-21")
        .timeout(PATIENCE)
        .json(body);
    let unreachable = |error: reqwest::Error| ApiError::bad_gateway(error.to_string());
    let response = request.send().await.map_err(unreachable)?;
    let status = response.status();
    let answer: Value = response.json().await.map_err(unreachable)?;
    if !status.is_success() {
        let message = answer["error"]["message"].as_str().unwrap_or("no message");
        return Err(ApiError::bad_gateway(format!(
            "Anthropic refused ({status}): {message}"
        )));
    }
    Ok(answer)
}

pub fn text(answer: &Value) -> String {
    let blocks = answer["content"].as_array().into_iter().flatten();
    blocks.filter_map(|block| block["text"].as_str()).collect()
}

pub fn stopped(answer: &Value) -> ApiError {
    ApiError::bad_gateway(format!(
        "The answer stopped early: {}",
        answer["stop_reason"]
    ))
}
