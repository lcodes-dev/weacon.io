//! A workspace's chats, which its members' machines run.

mod admit;
mod command;
mod link;
mod read;
mod reconnect;
#[cfg(test)]
mod test_support;
mod upload;
mod watch;

pub(crate) use reconnect::follow_machines;
pub(crate) use upload::chat as upload;
pub(crate) use watch::Watch;

use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tokio::sync::broadcast;
use weacon_protocol::{ChatId, ChatList, Message, WorkspaceId};

use crate::auth::Member;
use crate::http::{Shared, failure};

/// The most events of one chat sent at a time.
const PAGE: u32 = 200;
/// The bytes of a page of events, which leaves room in its frame for what travels with it.
const PAGE_BYTES: usize = weacon_protocol::MAX_FRAME - (16 << 10);

/// How many chats' new events a socket may fall behind by before it reads them all again.
const NOTICES: usize = 256;

/// What the server's connections share of its chats.
#[derive(Clone, Debug)]
pub(crate) struct Hub {
    stored: broadcast::Sender<(WorkspaceId, ChatId)>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            stored: broadcast::channel(NOTICES).0,
        }
    }
}

impl Hub {
    /// Tells the sockets that follow `chat` that it has new events.
    pub(crate) fn stored(&self, workspace: WorkspaceId, chat: ChatId) {
        // No socket follows it: no one to tell.
        drop(self.stored.send((workspace, chat)));
    }

    pub(crate) fn notices(&self) -> broadcast::Receiver<(WorkspaceId, ChatId)> {
        self.stored.subscribe()
    }
}

pub(crate) fn unsupported() -> weacon_observability::Fault {
    weacon_observability::Fault::new(
        weacon_protocol::code!("message.unsupported"),
        "This build doesn't know that message",
    )
}

/// Whether `message` is one a client sends about a chat.
pub(crate) const fn carries(message: &Message) -> bool {
    matches!(message, Message::ChatQuery(_) | Message::ChatCommand(_))
}

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/workspaces/{workspace}/chats", get(list))
}

/// `GET /api/workspaces/{workspace}/chats`: every chat in the workspace, the latest updated
/// first.
async fn list(member: Member) -> Response {
    match member.workspace.chats().await {
        Ok(chats) => Json(ChatList { chats }).into_response(),
        Err(fault) => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::ChatState;

    use super::*;
    use crate::db::test_support::{id, make_titled};
    use crate::test_support::{self, Fixture, bearer, request_with, run, status};

    async fn make(fixture: &Fixture, chat: &str, title: &str, at: i64) {
        let workspace = fixture.shared.db.workspace(&id("w1")).await.unwrap();
        make_titled(&workspace, chat, "m1", title, at)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_member_lists_the_workspaces_chats_and_their_titles_reach_no_log() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        make(&fixture, "c-1", "MARKER-TITLE-1", 10).await;
        make(&fixture, "c-2", "MARKER-TITLE-2", 20).await;
        let (_, body) = test_support::sign_in(&fixture, server.address, "ada@example.com").await;

        let (head, body) = request_with(
            server.address,
            "GET /api/workspaces/w1/chats",
            &bearer(&body),
            "",
        )
        .await;

        assert_eq!(status(&head), "http/1.1 200 ok");
        let list: ChatList = serde_json::from_str(&body).unwrap();
        let listed: Vec<_> = list
            .chats
            .iter()
            .map(|chat| (chat.id.as_str(), chat.title.as_str(), chat.state))
            .collect();
        assert_eq!(
            listed,
            [
                ("c-2", "MARKER-TITLE-2", ChatState::Open),
                ("c-1", "MARKER-TITLE-1", ChatState::Open)
            ]
        );
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn the_list_is_for_members_only() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        make(&fixture, "c-1", "Fix the build", 10).await;

        let (head, body) =
            request_with(server.address, "GET /api/workspaces/w1/chats", "", "").await;

        assert_eq!(status(&head), "http/1.1 401 unauthorized");
        assert!(!body.contains("c-1"), "{body}");
    }
}
