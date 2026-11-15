//! The daemon's answers to a client's chat commands and queries.

use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatAnswer, ChatCommand, ChatQuery, ChatsAnswer, ChatsAnswerKind, Envelope, LocalChat, Message,
};

use super::Shared;
use crate::app::blocking;
use crate::chats::engine::Listed;
use crate::chats::feed::Client;
use crate::chats::log;
use crate::session::Reply;

// The reply to a chat command: nothing when it was taken, since its receipts are events in the
// chat's log, or the error that refused it.
pub(super) async fn command_chat(
    shared: &Shared,
    id: u32,
    command: ChatCommand,
    client: &Client,
) -> Reply {
    let fields = [
        ("request_id", command.request.as_str().into()),
        ("workspace_id", command.workspace.as_str().into()),
        ("chat_id", command.chat.as_str().into()),
    ];
    match shared.chats.command(command, client).await {
        Ok(()) => {
            shared.log.info("chat command taken", &fields);
            Reply::NOTHING
        }
        Err(fault) => refused(shared, id, &fault),
    }
}

pub(super) async fn query_chat(
    shared: &Shared,
    id: u32,
    query: ChatQuery,
    client: &Client,
) -> Reply {
    let (workspace, chat) = (query.workspace.clone(), query.chat.clone());
    match shared.chats.query(query, client).await {
        Ok(answer) => Reply {
            envelope: answer.map(|answer| Envelope {
                id,
                message: Message::ChatAnswer(ChatAnswer {
                    workspace,
                    chat,
                    answer,
                }),
            }),
            close: false,
        },
        Err(fault) => refused(shared, id, &fault),
    }
}

// The chats this core serves, newest first, each titled from its log off the engine's loop.
pub(super) async fn list_chats(shared: &Shared, id: u32) -> Reply {
    let listed = match shared.chats.list().await {
        Ok(listed) => listed,
        Err(fault) => return refused(shared, id, &fault),
    };
    let titled = blocking(move || Ok(titled(listed))).await;
    let (chats, unreadable) = titled.unwrap_or_else(|error| (Vec::new(), Some(error)));
    if let Some(error) = unreadable {
        let fields = [("err", Field::error(&error))];
        shared.log.warn("chat titles unreadable", &fields);
    }
    Reply {
        envelope: Some(Envelope {
            id,
            message: Message::ChatsAnswer(ChatsAnswer {
                answer: ChatsAnswerKind::List { chats },
            }),
        }),
        close: false,
    }
}

// The chats `listed`, newest first, each titled from its log; one whose log can't be read is
// untitled, and the first such error comes back with them.
fn titled(listed: Vec<Listed>) -> (Vec<LocalChat>, Option<std::io::Error>) {
    let mut unreadable = None;
    let mut chats: Vec<_> = listed
        .into_iter()
        .map(|listed| LocalChat {
            title: log::title(&listed.root, &listed.key.chat).unwrap_or_else(|error| {
                unreadable.get_or_insert(error);
                String::new()
            }),
            workspace: listed.key.workspace,
            chat: listed.key.chat,
            agent: listed.agent,
            updated_at: i64::try_from(listed.active_at).unwrap_or(i64::MAX),
            run: listed.run,
        })
        .collect();
    chats.sort_by_key(|chat| std::cmp::Reverse(chat.updated_at));
    (chats, unreadable)
}

fn refused(shared: &Shared, id: u32, fault: &Fault) -> Reply {
    shared
        .log
        .warn("chat request refused", &[("err", Field::error(fault))]);
    Reply::error(id, fault.code().clone(), false)
}
