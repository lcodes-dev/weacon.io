use std::future::Future;
use std::io;

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    ChatAnswer, ChatCommand, ChatQuery, Envelope, LineDecoder, Message, Outcome, Pick, Receipt,
    code, encode_line,
};

use super::session::{Reply, Session};
use super::{HANDSHAKE_TIMEOUT, READ_CHUNK, Shared};
use crate::chats::feed::{self, Client};
use crate::picks::KeepError;
use crate::platform::Stream;

/// One client's connection: where it is in the conversation, and where events it subscribed to
/// are pushed.
struct Connection {
    session: Session,
    decoder: LineDecoder,
    client: Client,
}

pub(super) async fn serve_client(stream: Stream, shared: &Shared) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = [0; READ_CHUNK];
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let (push, mut pushed) = mpsc::channel(feed::QUEUE);
    let mut connection = Connection {
        session: Session::default(),
        decoder: LineDecoder::default(),
        client: Client { push },
    };
    loop {
        // Until the hello, reading counts against the deadline, as `answer`'s replies do.
        let limit = (!connection.session.greeted()).then_some(deadline);
        let read = tokio::select! {
            read = within(limit, reader.read(&mut buf)) => read,
            Some(message) = pushed.recv() => {
                if let Ok(line) = encode_line(&Envelope { id: 0, message }) {
                    writer.write_all(&line).await?;
                }
                continue;
            }
        };
        let Some(read) = read.transpose()? else {
            return Ok(());
        };
        if read == 0 {
            return Ok(());
        }
        let bytes = buf.get(..read).unwrap_or_default();
        if !answer(&mut connection, bytes, shared, &mut writer, deadline).await? {
            return Ok(());
        }
    }
}

// `future`'s output, or None when `deadline` comes first.
async fn within<F: Future>(deadline: Option<Instant>, future: F) -> Option<F::Output> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future).await.ok(),
        None => Some(future.await),
    }
}

// Answers what `bytes` completes; false once the connection should close. Replies before the
// hello count against `deadline`; what follows the hello, even in the same read, doesn't.
async fn answer(
    connection: &mut Connection,
    bytes: &[u8],
    shared: &Shared,
    writer: &mut (impl AsyncWrite + Unpin),
    deadline: Instant,
) -> io::Result<bool> {
    let Connection {
        session,
        decoder,
        client,
    } = connection;
    for received in decoder.push(bytes) {
        let limit = (!session.greeted()).then_some(deadline);
        if let Ok(Envelope {
            message: Message::Error(body),
            ..
        }) = &received
            && session.greeted()
        {
            let code = body.code.as_str().into();
            shared
                .log
                .warn("client reported an error", &[("code", code)]);
        }
        let reply = match received {
            Ok(Envelope {
                id,
                message: Message::Pick(pick),
            }) if session.greeted() => keep(shared, id, pick).await,
            Ok(Envelope {
                id,
                message: Message::ChatCommand(command),
            }) if session.greeted() => command_chat(shared, id, command).await,
            Ok(Envelope {
                id,
                message: Message::ChatQuery(query),
            }) if session.greeted() => query_chat(shared, id, query, client).await,
            received => session.answer(received, &shared.app),
        };
        if let Some(envelope) = reply.envelope
            && let Ok(line) = encode_line(&envelope)
        {
            let Some(written) = within(limit, writer.write_all(&line)).await else {
                return Ok(false);
            };
            written?;
        }
        if reply.close {
            return Ok(false);
        }
    }
    Ok(true)
}

// The reply to a chat command: nothing when it was taken, since its receipts are events in the
// chat's log, or the error that refused it.
async fn command_chat(shared: &Shared, id: u32, command: ChatCommand) -> Reply {
    let fields = [
        ("request_id", command.request.as_str().into()),
        ("workspace_id", command.workspace.as_str().into()),
        ("chat_id", command.chat.as_str().into()),
    ];
    match shared.chats.command(command).await {
        Ok(()) => {
            shared.log.info("chat command taken", &fields);
            Reply {
                envelope: None,
                close: false,
            }
        }
        Err(fault) => refused(shared, id, &fault),
    }
}

async fn query_chat(shared: &Shared, id: u32, query: ChatQuery, client: &Client) -> Reply {
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
    Reply::error(id, fault.code().clone(), false)
}

// Keeps `pick` off the runtime's thread and answers with its receipt.
async fn keep(shared: &Shared, id: u32, pick: Pick) -> Reply {
    let (picks, request) = (shared.picks.clone(), pick.request.clone());
    let kept = tokio::task::spawn_blocking(move || picks.keep(&pick))
        .await
        .unwrap_or_else(|error| Err(KeepError::Io(error.into())));
    let outcome = match kept {
        Ok(()) => {
            let request_id = request.as_str().into();
            shared
                .log
                .info("element picked", &[("request_id", request_id)]);
            Outcome::Completed
        }
        Err(KeepError::Conflict) => return Reply::error(id, code!("request.conflict"), false),
        Err(KeepError::Io(error)) => {
            shared
                .log
                .warn("pick not kept", &[("err", Field::error(&error))]);
            Outcome::Failed {
                code: code!("pick.not_kept"),
            }
        }
    };
    Reply {
        envelope: Some(Envelope {
            id,
            message: Message::Receipt(Receipt { request, outcome }),
        }),
        close: false,
    }
}
