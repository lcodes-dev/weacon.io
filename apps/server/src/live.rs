//! What changed, told to the browsers that have a page open. Only the kind of thing is
//! told, never the change: a browser then asks again for what it shows of that kind.

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use tokio::sync::broadcast;

use crate::db::{self, User};
use crate::{ApiError, AppState, auth};

/// How often a socket is checked to still have its session.
const PING: Duration = Duration::from_secs(30);

type News = (Vec<String>, &'static str);

/// The one channel every browser's socket listens to. It lives in memory only: a browser
/// connects again after the server restarts.
#[derive(Clone)]
pub struct Live(broadcast::Sender<News>);

impl Default for Live {
    fn default() -> Self {
        Self(broadcast::channel(1024).0)
    }
}

/// Tells these people that something of a kind changed: `chats`, `groups`, `tasks`, `docs`,
/// `projects` or `machines`.
pub fn tell(state: &AppState, people: Vec<String>, kind: &'static str) {
    state.live.0.send((people, kind)).ok();
}

pub async fn team(state: &AppState, team_id: &str, kind: &'static str) {
    let members = db::members(&state.pool, team_id).await;
    let people = members.unwrap_or_default().into_iter();
    tell(state, people.map(|member| member.id).collect(), kind);
}

/// Tells the team of a project.
pub async fn project(state: &AppState, project_id: &str, kind: &'static str) {
    let people = db::project_people(&state.pool, project_id).await;
    tell(state, people.unwrap_or_default(), kind);
}

/// Tells a group's members, and nobody else, since only they may know it is there.
pub async fn group(state: &AppState, group_id: &str) {
    let people = db::group_people(&state.pool, group_id).await;
    tell(state, people.unwrap_or_default(), "groups");
}

/// Tells who may read a chat: its team, or its group, which shows what its agent answers.
pub async fn chat(state: &AppState, chat_id: &str) {
    let found = db::chat_people(&state.pool, chat_id).await;
    let (people, grouped) = found.unwrap_or_default();
    if grouped {
        tell(state, people.clone(), "groups");
    }
    tell(state, people, "chats");
}

/// A browser's WebSocket.
pub async fn socket(
    State(state): State<AppState>,
    user: User,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    auth::own_page(&state, &headers)?;
    // Listened to from now, so that nothing is missed while the socket opens.
    let news = state.live.0.subscribe();
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
                Ok((people, kind)) if people.contains(&user) => Message::Text(kind.into()),
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
