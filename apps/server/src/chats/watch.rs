//! A member's chats on their workspace's socket: what they ask, and the chats they follow.

use std::collections::{HashMap, HashSet};

use tokio::sync::broadcast::{Receiver, error::RecvError};
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    Asker, ChatAnswer, ChatAnswerKind, ChatEntry, ChatId, ChatQuery, ChatQueryKind, ChatUpdate,
    Envelope, ErrorBody, MAX_FRAME, Message, WorkspaceId,
};

use super::{command, read};
use crate::auth::Member;
use crate::db::{Workspace, chat_not_found};
use crate::http::Shared;
use crate::socket::Closed;

/// The most events of one chat sent at a time.
const PAGE: u32 = 200;
// A page of events leaves room in its frames for what travels with them.
const PAGE_BYTES: usize = MAX_FRAME - (16 << 10);

/// The chats one socket follows, and which of them have events it hasn't sent yet.
pub(crate) struct Watch {
    notices: Receiver<(WorkspaceId, ChatId)>,
    /// Each chat followed, with the last of its events sent.
    following: HashMap<ChatId, u64>,
    behind: HashSet<ChatId>,
}

impl Watch {
    pub(crate) fn new(shared: &Shared) -> Self {
        Self {
            notices: shared.chats.notices(),
            following: HashMap::new(),
            behind: HashSet::new(),
        }
    }

    /// Waits until a chat it follows in `workspace` has new events; safe to cancel.
    pub(crate) async fn changed(&mut self, workspace: &WorkspaceId) -> Result<(), Closed> {
        loop {
            match self.notices.recv().await {
                Ok((place, chat)) if place == *workspace && self.following.contains_key(&chat) => {
                    self.behind.insert(chat);
                    return Ok(());
                }
                Ok(_) => {}
                // A notice missed may have been for any of them.
                Err(RecvError::Lagged(_)) => {
                    self.behind.extend(self.following.keys().cloned());
                    return Ok(());
                }
                Err(RecvError::Closed) => return Err(Closed),
            }
        }
    }

    /// The answer to `envelope`, a chat query or command from `asker`, under the id it
    /// answers; none for a subscription, whose events follow.
    pub(crate) async fn heard(
        &mut self,
        envelope: Envelope,
        member: &Member,
        asker: &Asker,
        shared: &Shared,
    ) -> Option<(u32, Message)> {
        let Envelope { id, message } = envelope;
        let named = if let Message::ChatQuery(query) = &message {
            &query.workspace
        } else if let Message::ChatCommand(asked) = &message {
            &asked.workspace
        } else {
            return None;
        };
        let elsewhere = named != member.workspace.id();
        let answered = if elsewhere {
            // Another workspace's chat answers as one there isn't.
            Err(chat_not_found())
        } else if let Message::ChatQuery(query) = message {
            self.query(member, query).await
        } else if let Message::ChatCommand(asked) = message {
            command::take(member, asker, shared, asked).await.map(Some)
        } else {
            return None;
        };
        let message = answered.unwrap_or_else(|fault| Some(refusal(shared, member, &fault)));
        message.map(|message| (id, message))
    }

    async fn query(&mut self, member: &Member, query: ChatQuery) -> Result<Option<Message>, Fault> {
        let workspace = &member.workspace;
        let answer = match query.query {
            ChatQueryKind::Subscribe { after } => {
                let chat = workspace
                    .chat(&query.chat)
                    .await?
                    .ok_or_else(chat_not_found)?;
                if after > chat.seq {
                    ChatAnswerKind::EventsLost
                } else {
                    self.following.insert(query.chat.clone(), after);
                    self.behind.insert(query.chat);
                    return Ok(None);
                }
            }
            kind @ (ChatQueryKind::Snapshot
            | ChatQueryKind::Window { .. }
            | ChatQueryKind::Newer) => read::answer(workspace, &query.chat, &kind).await?,
        };
        Ok(Some(Message::ChatAnswer(ChatAnswer {
            workspace: query.workspace,
            chat: query.chat,
            answer,
        })))
    }

    /// The next page of events of a chat it follows that is behind; empty once none is.
    pub(crate) async fn next(&mut self, workspace: &Workspace) -> Result<Vec<Message>, Fault> {
        while let Some(chat) = self.behind.iter().next().cloned() {
            let Some(&last) = self.following.get(&chat) else {
                self.behind.remove(&chat);
                continue;
            };
            let page = workspace
                .events_after(&chat, last, PAGE, PAGE_BYTES)
                .await?;
            let Some(ChatEntry { seq, .. }) = page.last() else {
                self.behind.remove(&chat);
                continue;
            };
            self.following.insert(chat.clone(), *seq);
            return Ok(page
                .into_iter()
                .map(|entry| {
                    Message::Chat(ChatUpdate {
                        workspace: workspace.id().clone(),
                        chat: chat.clone(),
                        entry,
                    })
                })
                .collect());
        }
        Ok(Vec::new())
    }
}

/// What the client hears of `fault`; only a failure of the server's own is logged.
fn refusal(shared: &Shared, member: &Member, fault: &Fault) -> Message {
    let code = fault.code();
    let theirs = !code.as_str().starts_with("db.");
    if !theirs {
        let workspace = member.workspace.id().as_str();
        let fields = [
            ("workspace_id", workspace.into()),
            ("err", Field::error(fault)),
        ];
        shared.log.warn("chat request failed", &fields);
    }
    Message::Error(ErrorBody {
        code: code.clone(),
        retryable: code.as_str() == "machine.offline" || !theirs,
    })
}

#[cfg(test)]
mod tests {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as Wire;
    use weacon_protocol::{Capability, ChatAction, ChatCommand, ChatState, RequestId, TaskCreate};

    use super::*;
    use crate::db::test_support::{NOW, append, id, make, said};
    use crate::test_support::socket::{Client, next, open, say};
    use crate::test_support::{self, Fixture, Running, pair_machine, request_with, run};

    async fn w1(fixture: &Fixture) -> Workspace {
        fixture.shared.db.workspace(&id("w1")).await.unwrap()
    }

    // Ada's session token, with a server running for it.
    async fn ada(fixture: &Fixture) -> (Running, String) {
        let server = run(fixture.shared.clone()).await;
        let (_, body) = test_support::sign_in(fixture, server.address, "ada@example.com").await;
        (server, body["token"].as_str().unwrap().to_owned())
    }

    async fn feed(server: &Running, token: &str, offers: &[&str]) -> Client {
        let mut client = open(server.address, "/api/workspaces/w1/feed", token)
            .await
            .unwrap();
        say(&mut client, offers).await;
        assert!(matches!(next(&mut client).await, Message::Welcome(_)));
        client
    }

    async fn ask(client: &mut Client, id: u32, message: Message) {
        let json = Envelope { id, message }.to_json().unwrap();
        client
            .send(Wire::Text(String::from_utf8(json).unwrap().into()))
            .await
            .unwrap();
    }

    // The next message about a chat, or an error, past the workspace's own changes.
    async fn heard(client: &mut Client) -> Message {
        loop {
            let message = next(client).await;
            if !matches!(message, Message::Workspace(_)) {
                return message;
            }
        }
    }

    fn query(workspace: &str, query: ChatQueryKind) -> Message {
        Message::ChatQuery(ChatQuery {
            workspace: id(workspace),
            chat: id("c-1"),
            query,
        })
    }

    fn seq_of(message: &Message) -> u64 {
        let Message::Chat(update) = message else {
            panic!("not a chat event: {message:?}");
        };
        update.entry.seq
    }

    fn refused(message: &Message) -> (&str, bool) {
        let Message::Error(ErrorBody { code, retryable }) = message else {
            panic!("not an error: {message:?}");
        };
        (code.as_str(), *retryable)
    }

    #[tokio::test]
    async fn a_member_follows_a_chat_from_any_place_in_its_log_and_hears_each_new_event() {
        let fixture = Fixture::new().await;
        let (server, token) = ada(&fixture).await;
        let workspace = w1(&fixture).await;
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        append(
            &workspace,
            "c-1",
            &[(Some(1), said("a")), (Some(2), said("b"))],
        )
        .await
        .unwrap();
        let mut client = feed(&server, &token, &["workspace.feed", "workspace.chats"]).await;

        ask(
            &mut client,
            2,
            query("w1", ChatQueryKind::Subscribe { after: 1 }),
        )
        .await;
        assert_eq!(seq_of(&heard(&mut client).await), 2);
        append(&workspace, "c-1", &[(Some(3), said("c"))])
            .await
            .unwrap();
        fixture.shared.chats.stored(id("w1"), id("c-1"));
        assert_eq!(seq_of(&heard(&mut client).await), 3);

        ask(&mut client, 3, query("w1", ChatQueryKind::Snapshot)).await;
        let Message::ChatAnswer(answer) = heard(&mut client).await else {
            panic!("no snapshot");
        };
        assert!(matches!(
            answer.answer,
            ChatAnswerKind::Snapshot { seq: 3, .. }
        ));
        ask(
            &mut client,
            4,
            query("w1", ChatQueryKind::Subscribe { after: 9 }),
        )
        .await;
        let Message::ChatAnswer(lost) = heard(&mut client).await else {
            panic!("not told of lost events");
        };
        assert_eq!(lost.answer, ChatAnswerKind::EventsLost);
        ask(&mut client, 5, query("w2", ChatQueryKind::Snapshot)).await;
        assert_eq!(
            refused(&heard(&mut client).await),
            ("chat.not_found", false)
        );
    }

    #[tokio::test]
    async fn a_client_that_doesnt_offer_chats_gets_no_answer_about_them() {
        let fixture = Fixture::new().await;
        let (server, token) = ada(&fixture).await;
        make(&w1(&fixture).await, "c-1", "m1", NOW).await.unwrap();
        let mut client = feed(&server, &token, &["workspace.feed", "workspace.tasks"]).await;
        assert!(matches!(next(&mut client).await, Message::Workspace(_)));

        ask(&mut client, 2, query("w1", ChatQueryKind::Snapshot)).await;
        let create = serde_json::to_string(&TaskCreate {
            title: "Write".to_owned(),
            before: None,
        })
        .unwrap();
        let auth = format!("authorization: Bearer {token}\r\n");
        request_with(
            server.address,
            "POST /api/workspaces/w1/tasks",
            &auth,
            &create,
        )
        .await;

        assert!(matches!(next(&mut client).await, Message::Workspace(_)));
    }

    fn prompt(chat: &str, machine: Option<&str>) -> Message {
        let machine = machine.map(id::<weacon_protocol::MachineId>);
        let asked = crate::db::test_support::prompt("r-9", chat, "MARKER-PROMPT", machine.as_ref());
        Message::ChatCommand(asked)
    }

    #[tokio::test]
    async fn a_command_reaches_only_the_owners_own_machine_and_none_is_connected_yet() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine["id"].as_str().unwrap().to_owned();
        let workspace = w1(&fixture).await;
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let mut client = feed(
            &server,
            &paired.session,
            &["workspace.feed", "workspace.chats"],
        )
        .await;

        ask(&mut client, 2, prompt("c-9", Some(&machine))).await;
        assert_eq!(
            refused(&heard(&mut client).await),
            ("machine.offline", true)
        );
        ask(&mut client, 3, prompt("c-9", Some("mc-other"))).await;
        assert_eq!(
            refused(&heard(&mut client).await),
            ("machine.not_found", false)
        );
        // c-1 runs on a machine that isn't paired any more.
        ask(&mut client, 4, prompt("c-1", None)).await;
        assert_eq!(refused(&heard(&mut client).await), ("chat.detached", false));

        let chat = workspace.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!(chat.state, ChatState::Detached);
        assert!(workspace.chat(&id("c-9")).await.unwrap().is_none());
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_member_whose_membership_ended_asks_nothing_more() {
        let fixture = Fixture::new().await;
        // No ping comes to find the membership ended before the prompt does.
        let mut quiet = fixture.shared.clone();
        quiet.sockets.ping_every = std::time::Duration::from_hours(1);
        let server = run(quiet).await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine["id"].as_str().unwrap().to_owned();
        let connected = crate::links::Connected {
            machine: id(&machine),
            owner: fixture
                .shared
                .db
                .account_with(&test_support::email("ada@example.com"))
                .await
                .unwrap()
                .unwrap(),
        };
        let serves = vec![Capability::from(weacon_protocol::code!("chat.serve"))];
        let mut attached = fixture.shared.links.open(connected, serves);
        let workspace = w1(&fixture).await;
        let mut client = feed(
            &server,
            &paired.session,
            &["workspace.feed", "workspace.chats"],
        )
        .await;
        // Past what the feed sends first, which ends it once the membership has.
        assert!(matches!(next(&mut client).await, Message::Workspace(_)));
        workspace.unlink(&test_support::member_id("m1")).await;

        ask(&mut client, 2, prompt("c-9", Some(&machine))).await;

        assert!(test_support::socket::ends(&mut client).await);
        attached.incoming.try_recv().unwrap_err();
        assert!(workspace.chat(&id("c-9")).await.unwrap().is_none());
        assert!(workspace.chat(&id("c-9")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_chats_client_hears_at_once_of_a_chat_made() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let paired = pair_machine(&fixture, server.address).await;
        let machine = paired.machine["id"].as_str().unwrap().to_owned();
        let connected = crate::links::Connected {
            machine: id(&machine),
            owner: fixture
                .shared
                .db
                .account_with(&test_support::email("ada@example.com"))
                .await
                .unwrap()
                .unwrap(),
        };
        let serves = vec![Capability::from(weacon_protocol::code!("chat.serve"))];
        let _attached = fixture.shared.links.open(connected, serves);
        let mut client = feed(
            &server,
            &paired.session,
            &["workspace.feed", "workspace.chats"],
        )
        .await;
        assert!(matches!(next(&mut client).await, Message::Workspace(_)));

        ask(&mut client, 2, prompt("c-9", Some(&machine))).await;

        assert!(matches!(next(&mut client).await, Message::Receipt(_)));
        let Message::Workspace(update) = next(&mut client).await else {
            panic!("the feed didn't hear of the chat");
        };
        assert_eq!(
            update.change,
            weacon_protocol::WorkspaceChange::ChatChanged { chat: id("c-9") }
        );
    }
}
