use std::process::Stdio;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::db::{self, User};
use crate::teams::{owned, refused};
use crate::{ApiError, AppState};

/// A page can take minutes to write.
const PATIENCE: Duration = Duration::from_secs(15 * 60);

/// The server's environment variables a program gets: enough to be found and signed in.
const KEPT: [&str; 5] = ["PATH", "HOME", "USER", "CODEX_HOME", "CLAUDE_CONFIG_DIR"];

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Kind {
    /// The Anthropic API, paid by the team's key.
    ClaudeApi,
    /// The `claude` program, on the account signed in on the server's computer.
    ClaudeCode,
    /// The `codex` program, likewise.
    Codex,
}

/// Who writes a team's docs.
#[derive(Deserialize, FromRow)]
pub struct Writer {
    pub kind: Kind,
    /// For the API. Missing in a request to keep the saved one.
    pub key: Option<String>,
    /// Empty for the cheap default.
    pub model: Option<String>,
}

impl Writer {
    fn model(&self) -> &str {
        let chosen = self.model.as_deref().filter(|model| !model.is_empty());
        chosen.unwrap_or(match self.kind {
            Kind::ClaudeApi | Kind::ClaudeCode => "claude-haiku-5-5",
            Kind::Codex => "gpt-5.6-luna",
        })
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
        match self.kind {
            Kind::ClaudeApi => self.ask_api(state, system, prompt).await,
            Kind::ClaudeCode => {
                // No tools, and none of the hooks, instructions and tool servers of the person's own setup.
                let args = [
                    "--print",
                    "--safe-mode",
                    "--model",
                    self.model(),
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--no-session-persistence",
                    "--system-prompt",
                    system,
                ];
                run(&state.claude_program, &args, prompt).await
            }
            Kind::Codex => {
                let model = self.model();
                // The sandbox only stops writing, so the tools that run commands are off too:
                // text in a repository must not be able to make Codex read the server's files.
                let args = [
                    "exec",
                    "--model",
                    model,
                    "--sandbox",
                    "read-only",
                    "--disable",
                    "shell_tool",
                    "--disable",
                    "unified_exec",
                    "--ignore-user-config",
                    "--ignore-rules",
                    "--skip-git-repo-check",
                    "--ephemeral",
                    "--color",
                    "never",
                    "-",
                ];
                // Codex takes no separate system prompt.
                run(
                    &state.codex_program,
                    &args,
                    &format!("{system}\n\n{prompt}"),
                )
                .await
            }
        }
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

/// Runs a program in an empty folder, with `input` on its input, and returns what it prints.
async fn run(program: &str, args: &[&str], input: &str) -> Result<String, String> {
    let folder = std::env::temp_dir().join(format!("weacon-writer-{:016x}", rand::random::<u64>()));
    tokio::fs::create_dir_all(&folder)
        .await
        .map_err(|error| error.to_string())?;
    let child = Command::new(program)
        .args(args)
        .current_dir(&folder)
        .env_clear()
        .envs(std::env::vars().filter(|(name, _)| KEPT.contains(&name.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let output = match child {
        Ok(mut child) => {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            let input = input.to_owned();
            // Written on the side, so a program that prints before it has read everything can't block us.
            tokio::spawn(async move { stdin.write_all(input.as_bytes()).await });
            match tokio::time::timeout(PATIENCE, child.wait_with_output()).await {
                Ok(output) => output.map_err(|error| error.to_string()),
                Err(_) => Err(format!("{program} took too long")),
            }
        }
        Err(error) => Err(format!("{program} could not be started: {error}")),
    };
    tokio::fs::remove_dir_all(&folder).await.ok();

    let output = output?;
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        return Ok(printed);
    }
    let complaint = String::from_utf8_lossy(&output.stderr);
    let complaint = if complaint.trim().is_empty() {
        &printed
    } else {
        complaint.trim()
    };
    // The end of it says what went wrong.
    let tail = complaint.char_indices().rev().nth(399);
    let tail = &complaint[tail.map_or(0, |(at, _)| at)..];
    Err(format!("{program} failed: {tail}"))
}

pub async fn save(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
    Json(mut writer): Json<Writer>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    if writer.kind == Kind::ClaudeApi {
        if writer.key.is_none() {
            writer.key = db::writer(&state.pool, team_id)
                .await?
                .and_then(|saved| saved.key);
        }
        if writer.key.is_none() {
            return Err(refused("An API key is needed"));
        }
    } else {
        // A program runs on the account of whoever runs the server.
        if !user.is_owner {
            let message = "Only the server's owner can use a program signed in on the server";
            return Err(ApiError(StatusCode::FORBIDDEN, message.into()));
        }
        writer.key = None;
    }
    db::save_writer(&state.pool, team_id, Some(&writer)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn forget(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    db::save_writer(&state.pool, team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
