//! A member's chats on their workspace's socket: what they ask, and the chats they follow.

use std::collections::{HashMap, HashSet};

use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::sync::mpsc;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    ChatAnswer, ChatAnswerKind, ChatEntry, ChatId, ChatQuery, ChatQueryKind, ChatUpdate, Envelope,
    ErrorBody, Message, WorkspaceId,
};

use super::{PAGE, PAGE_BYTES, read};
use crate::auth::Member;
use crate::db::{Workspace, chat_not_found};
use crate::shared::Shared;
use crate::socket::Closed;

/// The chats one socket follows, and which of them have events it hasn't sent yet.
pub(crate) struct Watch {
    /// Which chats have new events, for [`Watch::noticed`].
    pub(crate) notices: Receiver<(WorkspaceId, ChatId)>,
    /// Each chat followed, with the last of its events sent.
    following: HashMap<ChatId, u64>,
    behind: HashSet<ChatId>,
    /// Says, after each page, that the next may be waiting, so pages go out one per turn.
    pub(crate) more: mpsc::Receiver<()>,
    again: mpsc::Sender<()>,
}

impl Watch {
    pub(crate) fn new(shared: &Shared) -> Self {
        let (again, more) = mpsc::channel(1);
        Self {
            notices: shared.chats.notices(),
            following: HashMap::new(),
            behind: HashSet::new(),
            more,
            again,
        }
    }

    /// Takes in a notice from `notices`: whether a chat it follows in `workspace` is behind.
    pub(crate) fn noticed(
        &mut self,
        notice: Result<(WorkspaceId, ChatId), RecvError>,
        workspace: &WorkspaceId,
    ) -> Result<bool, Closed> {
        match notice {
            Ok((place, chat)) if place == *workspace && self.following.contains_key(&chat) => {
                self.behind.insert(chat);
                Ok(true)
            }
            Ok(_) => Ok(false),
            // A notice missed may have been for any of them.
            Err(RecvError::Lagged(_)) => {
                self.behind.extend(self.following.keys().cloned());
                Ok(true)
            }
            Err(RecvError::Closed) => Err(Closed),
        }
    }

    /// The answer to `envelope` when it is a chat query, under the id it answers; none for a
    /// subscription, whose events follow, or for anything else.
    pub(crate) async fn heard(
        &mut self,
        envelope: Envelope,
        member: &Member,
        shared: &Shared,
    ) -> Option<(u32, Message)> {
        let Envelope { id, message } = envelope;
        let Message::ChatQuery(query) = message else {
            return None;
        };
        let answered = if query.workspace == *member.workspace.id() {
            self.query(member, query).await
        } else {
            // Another workspace's chat answers as one there isn't.
            Err(chat_not_found())
        };
        let workspace = member.workspace.id();
        let message =
            answered.unwrap_or_else(|fault| Some(refusal(&shared.log, workspace, &fault)));
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
    /// After a page, [`Self::more`] hears that there may be another.
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
            // One is already waiting if this fails: the next turn takes the next page either way.
            let _waiting = self.again.try_send(());
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
fn refusal(log: &Logger, workspace: &WorkspaceId, fault: &Fault) -> Message {
    let code = fault.code();
    let ours = code.as_str().starts_with("db.");
    if ours {
        let fields = [
            ("workspace_id", workspace.as_str().into()),
            ("err", Field::error(fault)),
        ];
        log.warn("chat request failed", &fields);
    }
    Message::Error(ErrorBody {
        code: code.clone(),
        retryable: ours,
    })
}

#[cfg(test)]
mod tests {
    use weacon_protocol::TaskCreate;

    use super::*;
    use crate::db::test_support::{NOW, append, id, make, said};
    use crate::test_support::socket::{self, Socket, next, open, say};
    use crate::test_support::{self, Fixture, Running, request_with, run};

    async fn w1(fixture: &Fixture) -> Workspace {
        fixture.shared.db.workspace(&id("w1")).await.unwrap()
    }

    // Ada's session token, with a server running for it.
    async fn ada(fixture: &Fixture) -> (Running, String) {
        ada_on(fixture, fixture.shared.clone()).await
    }

    // Ada's session token, with a server running on `shared`.
    async fn ada_on(fixture: &Fixture, shared: Shared) -> (Running, String) {
        let server = run(shared).await;
        let (_, body) = test_support::sign_in(fixture, server.address, "ada@example.com").await;
        (server, body["token"].as_str().unwrap().to_owned())
    }

    async fn feed(server: &Running, token: &str, offers: &[&str]) -> Socket {
        let mut client = open(server.address, "/api/workspaces/w1/feed", token)
            .await
            .unwrap();
        say(&mut client, offers).await;
        assert!(matches!(next(&mut client).await, Message::Welcome(_)));
        client
    }

    async fn ask(client: &mut Socket, id: u32, message: Message) {
        socket::send(client, Envelope { id, message }).await;
    }

    // The next message about a chat, or an error, past the workspace's own changes.
    async fn heard(client: &mut Socket) -> Message {
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

    #[tokio::test]
    async fn a_member_whose_membership_ended_reads_nothing_more() {
        let fixture = Fixture::new().await;
        // No ping comes to find the membership ended before the query does.
        let mut quiet = fixture.shared.clone();
        quiet.sockets.ping_every = std::time::Duration::from_hours(1);
        let (server, token) = ada_on(&fixture, quiet).await;
        let workspace = w1(&fixture).await;
        let mut client = feed(&server, &token, &["workspace.feed", "workspace.chats"]).await;
        // Past what the feed sends first, which ends it once the membership has.
        assert!(matches!(next(&mut client).await, Message::Workspace(_)));
        workspace.unlink(&test_support::member_id("m1")).await;

        ask(&mut client, 2, query("w1", ChatQueryKind::Snapshot)).await;

        assert!(socket::ends(&mut client).await);
    }

    #[tokio::test]
    async fn a_subscription_at_the_chats_end_hears_the_next_event() {
        let fixture = Fixture::new().await;
        let (server, token) = ada(&fixture).await;
        let workspace = w1(&fixture).await;
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        append(&workspace, "c-1", &[(Some(1), said("a"))])
            .await
            .unwrap();
        let mut client = feed(&server, &token, &["workspace.feed", "workspace.chats"]).await;

        let at_end = query("w1", ChatQueryKind::Subscribe { after: 1 });
        ask(&mut client, 2, at_end).await;
        // Answered after the subscription, so the next event is stored only once it is taken.
        ask(&mut client, 3, query("w1", ChatQueryKind::Snapshot)).await;
        assert!(matches!(
            heard(&mut client).await,
            Message::ChatAnswer(ChatAnswer {
                answer: ChatAnswerKind::Snapshot { seq: 1, .. },
                ..
            })
        ));
        append(&workspace, "c-1", &[(Some(2), said("b"))])
            .await
            .unwrap();
        fixture.shared.chats.stored(id("w1"), id("c-1"));

        assert_eq!(seq_of(&heard(&mut client).await), 2);
    }

    #[tokio::test]
    async fn a_watch_is_behind_only_on_its_chats_in_its_workspace_or_when_it_missed_notices() {
        let fixture = Fixture::new().await;
        let mut watch = Watch::new(&fixture.shared);
        watch.following.insert(id("c-1"), 0);
        watch.following.insert(id("c-2"), 0);
        let w1 = id("w1");

        assert!(matches!(
            watch.noticed(Ok((id("w2"), id("c-1"))), &w1),
            Ok(false)
        ));
        assert!(matches!(
            watch.noticed(Ok((id("w1"), id("c-3"))), &w1),
            Ok(false)
        ));
        assert!(watch.behind.is_empty());
        assert!(matches!(
            watch.noticed(Ok((id("w1"), id("c-1"))), &w1),
            Ok(true)
        ));
        assert_eq!(watch.behind, HashSet::from([id("c-1")]));
        assert!(matches!(
            watch.noticed(Err(RecvError::Lagged(1)), &w1),
            Ok(true)
        ));
        assert_eq!(watch.behind, HashSet::from([id("c-1"), id("c-2")]));
        assert!(matches!(
            watch.noticed(Err(RecvError::Closed), &w1),
            Err(Closed)
        ));
    }

    #[test]
    fn only_the_servers_own_failures_are_logged_and_worth_retrying() {
        let (log, records) = test_support::recording_logger();
        let ours = Fault::new(weacon_protocol::code!("db.failed"), "The database failed");

        let told = refusal(&log, &id("w1"), &ours);
        assert_eq!(refused(&told), ("db.failed", true));
        assert_eq!(test_support::messages(&records), ["chat request failed"]);
        let told = refusal(&log, &id("w1"), &chat_not_found());
        assert_eq!(refused(&told), ("chat.not_found", false));
        assert_eq!(test_support::messages(&records).len(), 1);
    }

    #[tokio::test]
    async fn a_chat_longer_than_a_page_reaches_its_follower_page_by_page() {
        let fixture = Fixture::new().await;
        let (server, token) = ada(&fixture).await;
        let workspace = w1(&fixture).await;
        make(&workspace, "c-1", "m1", NOW).await.unwrap();
        let more = u64::from(PAGE) + 5;
        let events: Vec<_> = (1..=more).map(|seq| (Some(seq), said("a"))).collect();
        append(&workspace, "c-1", &events).await.unwrap();
        let mut client = feed(&server, &token, &["workspace.feed", "workspace.chats"]).await;

        let from_start = query("w1", ChatQueryKind::Subscribe { after: 0 });
        ask(&mut client, 2, from_start).await;

        for seq in 1..=more {
            assert_eq!(seq_of(&heard(&mut client).await), seq);
        }
    }
}
