use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Asker, ChatId, DownloadUrl, RequestId, WorkspaceId};

/// From a client to a core that offers `worktree.manage`: a question about a chat's worktree.
/// The answer is a [`WorktreeAnswer`] that echoes `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeQuery {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub query: WorktreeQueryKind,
    /// Who asked, when the server relays the query to a machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Asker")]
    pub by: Option<Asker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorktreeQueryKind {
    /// Whether the worktree is in the Trash, and what removing it would set aside.
    State,
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From a client to a core that offers `worktree.manage`: what a person asks of a chat's
/// worktree. The core acts only for someone who may act on the chat, and a client asks the
/// person before `remove`, whatever the access level. The answer is a [`WorktreeAnswer`] that
/// echoes `request`: the worktree's state after `remove` or `restore`, which a repeated one
/// answers too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeCommand {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub action: WorktreeAction,
    /// Who asked, when the server relays the command to a machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Asker")]
    pub by: Option<Asker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorktreeAction {
    /// Moves the worktree to the Trash; its branch stays.
    Remove,
    /// Brings the worktree back from the Trash to where it was.
    Restore,
    /// Pushes the chat's branch and opens a draft pull request from it.
    OpenPullRequest { title: String, body: String },
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From the core to a client that has `worktree.manage`, in answer to a [`WorktreeQuery`] or a
/// [`WorktreeCommand`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeAnswer {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub answer: WorktreeAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorktreeAnswerKind {
    /// Where the worktree stands, in the Trash or not. A chat with no worktree yet gets the
    /// error `worktree.none`.
    State {
        /// In the Trash.
        removed: bool,
        uncommitted: bool,
        unpushed: bool,
    },
    /// The branch was pushed and a draft pull request opened at `url`.
    PullRequestOpened { url: DownloadUrl },
    /// The branch was pushed, but no pull request could be opened from here; `url` is the
    /// page where the person can open one.
    PullRequestPage { url: DownloadUrl },
    /// An answer this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{AccountId, Envelope, FrameError, MemberId, Message};

    fn request() -> RequestId {
        RequestId::try_from("r-1".to_owned()).unwrap()
    }

    fn workspace() -> WorkspaceId {
        WorkspaceId::try_from("w-1".to_owned()).unwrap()
    }

    fn chat() -> ChatId {
        ChatId::try_from("c-1".to_owned()).unwrap()
    }

    fn wire(message: &str, field: &str, body: Value) -> Value {
        let mut wire = json!({ "id": 4, "type": message, "request": "r-1",
                               "workspace": "w-1", "chat": "c-1" });
        wire[field] = body;
        wire
    }

    fn travels(message: Message, wire: Value) {
        let envelope = Envelope { id: 4, message };
        assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
        assert_eq!(
            Envelope::from_json(&envelope.to_json().unwrap()).unwrap(),
            envelope
        );
        assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), envelope);
    }

    fn answer(answer: WorktreeAnswerKind) -> Message {
        Message::WorktreeAnswer(WorktreeAnswer {
            request: request(),
            workspace: workspace(),
            chat: chat(),
            answer,
        })
    }

    #[test]
    fn queries_and_commands_have_a_tagged_shape() {
        travels(
            Message::WorktreeQuery(WorktreeQuery {
                request: request(),
                workspace: workspace(),
                chat: chat(),
                query: WorktreeQueryKind::State,
                by: None,
            }),
            wire("worktree_query", "query", json!({ "kind": "state" })),
        );
        let actions = [
            (WorktreeAction::Remove, json!({ "kind": "remove" })),
            (WorktreeAction::Restore, json!({ "kind": "restore" })),
            (
                WorktreeAction::OpenPullRequest {
                    title: "Fix the build".to_owned(),
                    body: "Pins the toolchain.".to_owned(),
                },
                json!({ "kind": "open_pull_request", "title": "Fix the build",
                        "body": "Pins the toolchain." }),
            ),
        ];
        for (action, body) in actions {
            let command = WorktreeCommand {
                request: request(),
                workspace: workspace(),
                chat: chat(),
                action,
                by: None,
            };
            travels(
                Message::WorktreeCommand(command),
                wire("worktree_command", "action", body),
            );
        }
    }

    #[test]
    fn answers_tag_each_kind() {
        let link = "https://github.com/acme/site/pull/7";
        let url = DownloadUrl::try_from(link.to_owned()).unwrap();
        let cases = [
            (
                WorktreeAnswerKind::State {
                    removed: false,
                    uncommitted: true,
                    unpushed: false,
                },
                json!({ "kind": "state", "removed": false, "uncommitted": true,
                        "unpushed": false }),
            ),
            (
                WorktreeAnswerKind::PullRequestOpened { url: url.clone() },
                json!({ "kind": "pull_request_opened", "url": link }),
            ),
            (
                WorktreeAnswerKind::PullRequestPage { url },
                json!({ "kind": "pull_request_page", "url": link }),
            ),
        ];
        for (kind, body) in cases {
            travels(answer(kind), wire("worktree_answer", "answer", body));
        }
    }

    #[test]
    fn a_pull_requests_link_is_only_a_plain_https_address() {
        let refused = [
            "javascript:alert(1)",
            "http://github.com/acme/site/pull/7",
            "https://user:token@github.com/acme/site/pull/7",
        ];
        for link in refused {
            let wire = wire(
                "worktree_answer",
                "answer",
                json!({ "kind": "pull_request_opened", "url": link }),
            );
            serde_json::from_value::<Envelope>(wire).unwrap_err();
        }
    }

    #[test]
    fn a_relayed_query_or_command_says_who_asked() {
        let by = Some(Asker {
            account: AccountId::try_from("a-1".to_owned()).unwrap(),
            member: MemberId::try_from("m-1".to_owned()).unwrap(),
        });
        let asked = json!({ "account": "a-1", "member": "m-1" });
        let mut query = wire("worktree_query", "query", json!({ "kind": "state" }));
        query["by"] = asked.clone();
        travels(
            Message::WorktreeQuery(WorktreeQuery {
                request: request(),
                workspace: workspace(),
                chat: chat(),
                query: WorktreeQueryKind::State,
                by: by.clone(),
            }),
            query,
        );
        let mut command = wire("worktree_command", "action", json!({ "kind": "remove" }));
        command["by"] = asked;
        travels(
            Message::WorktreeCommand(WorktreeCommand {
                request: request(),
                workspace: workspace(),
                chat: chat(),
                action: WorktreeAction::Remove,
                by,
            }),
            command,
        );
    }

    #[test]
    fn a_newer_peers_worktree_messages_parse_and_are_never_sent() {
        let newer = [
            wire("worktree_query", "query", json!({ "kind": "history" })),
            wire("worktree_command", "action", json!({ "kind": "merge" })),
            wire("worktree_answer", "answer", json!({ "kind": "merged" })),
        ];
        for wire in newer {
            let envelope: Envelope = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{wire}");
        }
    }

    #[test]
    fn a_known_action_without_its_fields_is_refused() {
        let wire = wire(
            "worktree_command",
            "action",
            json!({ "kind": "open_pull_request" }),
        );
        serde_json::from_value::<Envelope>(wire).unwrap_err();
    }
}
