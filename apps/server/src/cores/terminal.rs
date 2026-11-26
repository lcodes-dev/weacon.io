//! The sign-in terminals: what an agent's own sign-in shows on a machine, handed on to the
//! browser that opened it and back, and kept nowhere.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::http::HeaderMap;
use core_link::{Agent, FromCore, ToCore};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::{AppState, auth};

/// How often the terminal is checked to still have its session.
const PING: Duration = Duration::from_secs(30);

/// What a browser sends to a terminal.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Typed {
    Data { data: String },
    Size { cols: u16, rows: u16 },
}

pub async fn relay(
    state: AppState,
    headers: HeaderMap,
    machine: String,
    agent: Agent,
    mut socket: WebSocket,
    mut from_core: mpsc::UnboundedReceiver<FromCore>,
) {
    let (cols, rows) = (80, 24);
    state
        .cores
        .send(&machine, ToCore::TerminalOpen { agent, cols, rows });
    let mut still = tokio::time::interval(PING);
    loop {
        tokio::select! {
            // The terminal ends with the session it was opened in.
            _ = still.tick() => {
                if !matches!(auth::signed_in(&state, &headers).await, Ok(Some(_))) {
                    break;
                }
            }
            message = from_core.recv() => match message {
                Some(FromCore::TerminalData { data, .. }) => {
                    let shown = json!({"type": "data", "data": data}).to_string();
                    if socket.send(Message::Text(shown.into())).await.is_err() {
                        break;
                    }
                }
                // The sign-in ended, or the core went away.
                _ => break,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    let message = match serde_json::from_str(&text) {
                        Ok(Typed::Data { data }) => ToCore::TerminalData { agent, data },
                        Ok(Typed::Size { cols, rows }) => ToCore::TerminalSize { agent, cols, rows },
                        Err(_) => continue,
                    };
                    state.cores.send(&machine, message);
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    state.cores.close_terminal(&machine, agent);
    state.cores.send(&machine, ToCore::TerminalClose { agent });
}
