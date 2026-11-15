//! The daemon's answers to a client's chat commands and queries.

use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatAnswer, ChatCommand, ChatQuery, Envelope, ErrorBody, Message, code};

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
    refusal(id, fault)
}

// The error a request `fault` refused gets, worth sending again when it was cut short.
fn refusal(id: u32, fault: &Fault) -> Reply {
    let code = fault.code().clone();
    let retryable = code == code!("chat.unfinished");
    Reply {
        envelope: Some(Envelope {
            id,
            message: Message::Error(ErrorBody { code, retryable }),
        }),
        close: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Whether the reply to request 7, refused with `code`, is worth sending again.
    fn sent_again(code: &weacon_protocol::Code) -> bool {
        let reply = refusal(7, &Fault::new(code.clone(), "Refused"));
        assert!(!reply.close);
        let Some(Envelope {
            id: 7,
            message: Message::Error(error),
        }) = reply.envelope
        else {
            panic!("no error");
        };
        assert_eq!(&error.code, code);
        error.retryable
    }

    #[test]
    fn only_a_request_cut_short_is_worth_sending_again_and_neither_closes() {
        assert!(sent_again(&code!("chat.unfinished")));
        assert!(!sent_again(&code!("chat.busy")));
    }
}
