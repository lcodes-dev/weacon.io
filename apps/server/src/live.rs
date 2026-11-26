use tokio::sync::broadcast;

use crate::{AppState, db};

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
    let people = db::teams::member_ids(&state.pool, team_id).await;
    tell(state, people.unwrap_or_default(), topic);
}

pub async fn project(state: &AppState, project_id: &str, topic: Topic) {
    let people = db::projects::people(&state.pool, project_id).await;
    tell(state, people.unwrap_or_default(), topic);
}

pub async fn group(state: &AppState, group_id: &str) {
    let people = db::groups::people(&state.pool, group_id).await;
    tell(state, people.unwrap_or_default(), Topic::Groups);
}

pub async fn chat(state: &AppState, chat_id: &str) {
    let found = db::chats::people(&state.pool, chat_id).await;
    let (people, grouped) = found.unwrap_or_default();
    if grouped {
        tell(state, people.clone(), Topic::Groups);
    }
    tell(state, people, Topic::Chats);
}
