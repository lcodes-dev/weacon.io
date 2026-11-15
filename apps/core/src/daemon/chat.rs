//! The daemon's answers to a client's chat commands and queries.

use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatAnswer, ChatCommand, ChatQuery, Envelope, Message, code};

use super::Shared;
use crate::chats::feed::Client;
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

fn refused(shared: &Shared, id: u32, fault: &Fault) -> Reply {
    shared
        .log
        .warn("chat request refused", &[("err", Field::error(fault))]);
    Reply::error(id, fault.code().clone(), again(fault))
}

// Whether a request `fault` refused is worth sending again: one cut short is.
fn again(fault: &Fault) -> bool {
    *fault.code() == code!("chat.unfinished")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_request_cut_short_is_worth_sending_again() {
        let cut_short = Fault::new(code!("chat.unfinished"), "The command was cut short");
        assert!(again(&cut_short));
        let refused = Fault::new(code!("chat.busy"), "The agent is busy");
        assert!(!again(&refused));
    }
}
