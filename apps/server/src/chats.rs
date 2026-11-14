//! A workspace's chats, which its members' machines run.

mod admit;

use axum::Json;
use axum::response::{IntoResponse, Response};
use weacon_protocol::ChatList;

use crate::auth::Member;
use crate::http::failure;

/// `GET /api/workspaces/{workspace}/chats`: every chat in the workspace, the latest updated
/// first.
pub(crate) async fn list(member: Member) -> Response {
    match member.workspace.chats().await {
        Ok(chats) => Json(ChatList { chats }).into_response(),
        Err(fault) => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatId, ChatState, MachineId, MachineName, RequestId, WorkspaceId};

    use super::*;
    use crate::db::{NewChat, NewRequest};
    use crate::test_support::{self, Fixture, bearer, member_id, request_with, run, status};

    async fn make(fixture: &Fixture, chat: &str, title: &str, at: i64) {
        let workspace = fixture
            .shared
            .db
            .workspace(&WorkspaceId::try_from("w1".to_owned()).unwrap())
            .await
            .unwrap();
        let (chat, owner) = (ChatId::try_from(chat.to_owned()).unwrap(), member_id("m1"));
        let request = RequestId::try_from(format!("r-{}", chat.as_str())).unwrap();
        let machine = MachineName::try_from("Ada's laptop".to_owned()).unwrap();
        let machine_id = MachineId::try_from("mc-1".to_owned()).unwrap();
        let new = NewChat {
            id: &chat,
            owner: &owner,
            title,
            repo: None,
            machine_id: &machine_id,
            machine: &machine,
            at,
        };
        let first = NewRequest {
            id: &request,
            chat: &chat,
            member: &owner,
            action_hash: b"h",
            at,
        };
        workspace.add_chat(&new, &first).await.unwrap();
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
