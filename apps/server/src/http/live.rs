use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use tokio::sync::broadcast;

use crate::AppState;
use crate::auth;
use crate::db::users::User;
use crate::error::Result;
use crate::live::News;

/// How often a socket is checked to still have its session.
const PING: Duration = Duration::from_secs(30);

/// A browser's WebSocket, which hears the topics that changed for its user.
pub async fn connect(
    State(state): State<AppState>,
    user: User,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response> {
    auth::own_page(&state, &headers)?;
    // Listened to from now, so that nothing is missed while the socket opens.
    let news = state.live.subscribe();
    Ok(socket.on_upgrade(move |socket| serve(state, headers, user.id, socket, news)))
}

async fn serve(
    state: AppState,
    headers: HeaderMap,
    user: String,
    mut socket: WebSocket,
    mut news: broadcast::Receiver<News>,
) {
    let mut still = tokio::time::interval(PING);
    loop {
        let send = tokio::select! {
            // The socket ends with the session it was opened in.
            _ = still.tick() => match auth::signed_in(&state, &headers).await {
                Ok(Some(_)) => Message::Ping(Default::default()),
                _ => break,
            },
            told = news.recv() => match told {
                Ok((people, topic)) if people.contains(&user) => Message::Text(topic.name().into()),
                Ok(_) => continue,
                // It fell behind, and asks for everything again when it connects anew.
                Err(_) => break,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            },
        };
        if socket.send(send).await.is_err() {
            break;
        }
    }
}
