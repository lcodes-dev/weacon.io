use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use tokio::sync::broadcast;

use crate::accounts::db::User;
use crate::error::Result;
use crate::{AppState, accounts, auth, chats, groups, projects};

#[derive(Clone, Copy)]
pub enum Topic {
    Chats,
    Docs,
    Groups,
    Machines,
    Projects,
    Tasks,
}

impl Topic {
    pub fn name(self) -> &'static str {
        match self {
            Topic::Chats => "chats",
            Topic::Docs => "docs",
            Topic::Groups => "groups",
            Topic::Machines => "machines",
            Topic::Projects => "projects",
            Topic::Tasks => "tasks",
        }
    }
}

pub type News = (Vec<String>, Topic);

#[derive(Clone)]
pub struct Live(broadcast::Sender<News>);

impl Default for Live {
    fn default() -> Self {
        Self(broadcast::channel(1024).0)
    }
}

impl Live {
    pub fn subscribe(&self) -> broadcast::Receiver<News> {
        self.0.subscribe()
    }
}

fn tell(state: &AppState, people: Vec<String>, topic: Topic) {
    state.live.0.send((people, topic)).ok();
}

pub fn user(state: &AppState, user_id: &str, topic: Topic) {
    tell(state, vec![user_id.to_owned()], topic);
}

pub async fn team(state: &AppState, team_id: &str, topic: Topic) {
    let people = accounts::db::member_ids(&state.pool, team_id).await;
    tell(state, people.unwrap_or_default(), topic);
}

pub async fn project(state: &AppState, project_id: &str, topic: Topic) {
    let people = projects::db::people(&state.pool, project_id).await;
    tell(state, people.unwrap_or_default(), topic);
}

pub async fn group(state: &AppState, group_id: &str) {
    let people = groups::db::people(&state.pool, group_id).await;
    tell(state, people.unwrap_or_default(), Topic::Groups);
}

pub async fn chat(state: &AppState, chat_id: &str) {
    let found = chats::db::people(&state.pool, chat_id).await;
    let (people, grouped) = found.unwrap_or_default();
    if grouped {
        tell(state, people.clone(), Topic::Groups);
    }
    tell(state, people, Topic::Chats);
}

const PING: Duration = Duration::from_secs(30);

pub async fn connect(
    State(state): State<AppState>,
    user: User,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Result<Response> {
    auth::own_page(&state, &headers)?;
    let news = state.live.subscribe();
    Ok(socket.on_upgrade(move |socket| serve_socket(state, headers, user.id, socket, news)))
}

async fn serve_socket(
    state: AppState,
    headers: HeaderMap,
    user: String,
    mut socket: WebSocket,
    mut news: broadcast::Receiver<News>,
) {
    let mut still = tokio::time::interval(PING);
    loop {
        let send = tokio::select! {
            _ = still.tick() => match auth::signed_in(&state, &headers).await {
                Ok(Some(_)) => Message::Ping(Default::default()),
                _ => break,
            },
            told = news.recv() => match told {
                Ok((people, topic)) if people.contains(&user) => Message::Text(topic.name().into()),
                Ok(_) => continue,
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
