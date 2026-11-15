use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentKind, ChatId, RunState, WorkspaceId};

/// A local client asks the core which chats it serves, in every workspace, under `chat.list`.
/// Never relayed by the server, which lists a workspace's chats itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatsQuery {
    pub query: ChatsQueryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatsQueryKind {
    /// Every chat this core has run or queued a prompt for, newest first.
    List,
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// The core's answer to a [`ChatsQuery`], under the query's envelope id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatsAnswer {
    pub answer: ChatsAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatsAnswerKind {
    List {
        chats: Vec<LocalChat>,
    },
    /// An answer this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// A chat a core serves: `title` is its first prompt's first line, as the server titles it, and
/// `updated_at` is when its latest turn began or ended, in seconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalChat {
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub title: String,
    pub agent: AgentKind,
    pub updated_at: i64,
    /// What its latest run is doing; left out when it has had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "RunState")]
    pub run: Option<RunState>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Message;
    use crate::test_support::{chat, envelope, never_sent, travels, workspace};

    #[test]
    fn a_list_query_and_its_answer_have_a_tagged_shape() {
        let query = envelope(Message::ChatsQuery(ChatsQuery {
            query: ChatsQueryKind::List,
        }));
        travels(
            &query,
            json!({ "id": 4, "type": "chats_query", "query": { "kind": "list" } }),
        );

        let fresh = LocalChat {
            workspace: workspace(),
            chat: chat(),
            title: "Fix the build".to_owned(),
            agent: AgentKind::Claude,
            updated_at: 1_700_000_000,
            run: None,
        };
        let ran = LocalChat {
            run: Some(RunState::Waiting),
            ..fresh.clone()
        };
        let answer = envelope(Message::ChatsAnswer(ChatsAnswer {
            answer: ChatsAnswerKind::List {
                chats: vec![ran, fresh],
            },
        }));
        let chat = json!({ "workspace": "w-1", "chat": "c-1", "title": "Fix the build",
                           "agent": "claude", "updated_at": 1_700_000_000 });
        let mut waiting = chat.clone();
        waiting["run"] = json!("waiting");
        travels(
            &answer,
            json!({ "id": 4, "type": "chats_answer",
                    "answer": { "kind": "list", "chats": [waiting, chat] } }),
        );
    }

    #[test]
    fn a_newer_peers_query_and_answer_parse_and_are_never_sent() {
        never_sent(json!({ "id": 4, "type": "chats_query", "query": { "kind": "search" } }));
        never_sent(json!({ "id": 4, "type": "chats_answer", "answer": { "kind": "page" } }));
    }
}
