//! A fake server that answers a web client's hello on a WebSocket, then streams a recorded run
//! to it, one message per text frame, keeps what the client sends, accepts its prompts and
//! answers its diff queries with a made-up change of 40 files.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::{self, Message as Frame};
use weacon_protocol::{Capability, Code, Envelope, ErrorBody, Message, code};

use crate::Failure;
use crate::fake_chat::FakeChat;
use crate::fake_worktree::FakeWorktree;
use crate::recording::envelope_text;
use crate::replay::{BUFFER, play};
use crate::{fake_core, fake_diff};

const OFFERED: &[Capability] = &[
    Capability::new(code!("log.forward")),
    Capability::new(code!("workspace.chats")),
    Capability::new(code!("workspace.chat_commands")),
    Capability::new(code!("diff.view")),
    Capability::new(code!("worktree.manage")),
];

/// Plays `recording` to the client on `stream`, waiting `pace` after each message, and appends
/// each text frame the client sends after its hello to `received`, one per line. It offers
/// `workspace.chats` and `workspace.chat_commands`, committing a receipt that accepts each
/// command to the chat, `diff.view` and `worktree.manage`; it refuses a message it doesn't know.
///
/// # Errors
/// When the handshake fails, the client's first frame isn't a hello on this protocol that offers
/// `run.stream`, or the connection fails.
pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    recording: String,
    pace: Duration,
    received: &Path,
) -> Result<(), Failure> {
    let socket = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut frames) = socket.split();
    let hello = next_text(&mut frames)
        .await?
        .ok_or("the client left before its hello")?;
    let answer = fake_core::answer(&hello, OFFERED)?;
    let welcomed = matches!(answer.message, Message::Welcome(_));
    sink.send(Frame::text(envelope_text(&answer)?)).await?;
    if !welcomed {
        return Err("the client speaks another protocol".into());
    }
    let chat = Mutex::new(FakeChat::new()?);
    let (run_out, run_lines) = duplex(BUFFER);
    let (reply_sender, replies) = unbounded_channel();
    let playing = play(recording, BufReader::new(tokio::io::empty()), run_out, pace);
    let committed = reply_sender.clone();
    let sending = async {
        drop(tokio::join!(
            playing,
            send_all(&mut sink, run_lines, replies, &chat, &committed)
        ));
    };
    let keeping = keep(&mut frames, received, &chat, reply_sender);
    tokio::pin!(keeping);
    tokio::select! {
        kept = &mut keeping => kept,
        () = sending => keeping.await,
    }
}

// Sends each of the run's lines the chat still plays, and each reply, as a text frame, until a
// send fails, such as when the client leaves mid-stream; once the lines end, their run asks.
async fn send_all(
    sink: &mut (impl Sink<Frame> + Unpin),
    run_lines: impl AsyncRead + Unpin,
    mut replies: UnboundedReceiver<String>,
    chat: &Mutex<FakeChat>,
    committed: &UnboundedSender<String>,
) {
    let mut run_lines = BufReader::new(run_lines).lines();
    let mut playing = true;
    loop {
        let reply = tokio::select! {
            line = run_lines.next_line(), if playing => {
                if let Ok(Some(line)) = line {
                    commit_played(chat, committed, line);
                } else {
                    playing = false;
                    ended(chat, committed);
                }
                continue;
            }
            Some(reply) = replies.recv() => reply,
            else => break,
        };
        if sink.send(Frame::text(reply)).await.is_err() {
            return;
        }
    }
}

// Appends each text frame to `received` until the client closes, and hands what answers each
// to `replies`.
async fn keep(
    frames: &mut (impl Stream<Item = tungstenite::Result<Frame>> + Unpin),
    received: &Path,
    chat: &Mutex<FakeChat>,
    replies: UnboundedSender<String>,
) -> Result<(), Failure> {
    let mut worktree = FakeWorktree::default();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(received)
        .await?;
    while let Some(text) = next_text(frames).await? {
        file.write_all(format!("{text}\n").as_bytes()).await?;
        // Tokio's file writes in the background; a reader may look at any moment.
        file.flush().await?;
        reply_to(&text, chat, &mut worktree, &replies);
    }
    Ok(())
}

// Sends what the chat committed with a played line, then the line, under the chat's lock so they
// keep their order; nothing once its run is stopped.
fn commit_played(chat: &Mutex<FakeChat>, replies: &UnboundedSender<String>, line: String) {
    let Ok(envelope) = Envelope::from_json(line.as_bytes()) else {
        drop(replies.send(line));
        return;
    };
    if let Ok(mut chat) = chat.lock()
        && let Some(told) = chat.played(&envelope)
    {
        send_texts(replies, &told);
        drop(replies.send(line));
    }
}

// Once the recording has played, its run asks to use Bash.
fn ended(chat: &Mutex<FakeChat>, replies: &UnboundedSender<String>) {
    if let Ok(mut chat) = chat.lock() {
        send_texts(replies, &chat.ended());
    }
}

fn send_texts(replies: &UnboundedSender<String>, envelopes: &[Envelope]) {
    for text in envelopes
        .iter()
        .filter_map(|envelope| envelope_text(envelope).ok())
    {
        // This fails only once `serve` is done with the client.
        drop(replies.send(text));
    }
}

// Sends what answers `text` to `replies`: the answers to a diff query, the worktree's answer, and
// what the chat says to a query or a command, its receipt included.
fn reply_to(
    text: &str,
    chat: &Mutex<FakeChat>,
    worktree: &mut FakeWorktree,
    replies: &UnboundedSender<String>,
) {
    let Ok(envelope) = Envelope::from_json(text.as_bytes()) else {
        return;
    };
    if envelope.message == Message::Unknown {
        let refused = refusal(envelope.id, code!("message.unsupported"));
        return send_texts(replies, &[refused]);
    }
    send_texts(replies, &fake_diff::answers(&envelope));
    send_texts(replies, worktree.answer(&envelope).as_slice());
    if let Ok(mut chat) = chat.lock() {
        send_texts(replies, &chat.answer(&envelope));
    }
}

/// The error that answers the message `id`.
pub(crate) const fn refusal(id: u32, code: Code) -> Envelope {
    Envelope {
        id,
        message: Message::Error(ErrorBody {
            code,
            retryable: false,
        }),
    }
}

// The next text frame, skipping pings and pongs; `None` once the connection ends. Reading past
// a close sends its reply, so the client sees a clean close.
async fn next_text(
    frames: &mut (impl Stream<Item = tungstenite::Result<Frame>> + Unpin),
) -> Result<Option<String>, Failure> {
    while let Some(frame) = frames.next().await {
        match frame? {
            Frame::Text(text) => return Ok(Some(text.as_str().to_owned())),
            Frame::Binary(_) => return Err("the client sent a binary frame".into()),
            Frame::Close(_) | Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_) => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async, connect_async};

    use super::*;
    use weacon_protocol::{
        ChatAction, ChatCommand, ChatId, ChatQuery, ChatQueryKind, DiffQuery, DiffQueryKind,
        Outcome, PROTOCOL, PermissionAnswer, RequestId, RunEvent, RunId, RunUpdate, WorkspaceId,
    };

    use crate::TempDir;
    use crate::fake_chat::SEEDED;
    use crate::fake_core::client_hello;
    use crate::recording::{TRANSCRIPT, recording};

    type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

    async fn start(
        recording: String,
        received: PathBuf,
    ) -> (Client, tokio::task::JoinHandle<Result<(), Failure>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve(stream, recording, Duration::ZERO, &received).await
        });
        (connect_async(url).await.unwrap().0, served)
    }

    fn hello(protocol: u32) -> Frame {
        let hello = client_hello(1, protocol, [code!("run.stream")]);
        Frame::text(envelope_text(&hello).unwrap())
    }

    // The texts the client gets, up to the first that `done` holds for.
    async fn until(client: &mut Client, done: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = texts(client, 1).await;
        while !seen.last().is_some_and(&done) {
            seen.extend(texts(client, 1).await);
        }
        seen
    }

    async fn texts(
        client: &mut (impl Stream<Item = tungstenite::Result<Frame>> + Unpin),
        count: usize,
    ) -> Vec<Value> {
        let mut texts = Vec::new();
        while texts.len() < count {
            if let Frame::Text(text) = client.next().await.unwrap().unwrap() {
                texts.push(serde_json::from_str(text.as_str()).unwrap());
            }
        }
        texts
    }

    #[tokio::test]
    async fn streams_the_recording_one_message_per_frame_and_keeps_what_the_client_sends() {
        let dir = TempDir::new().unwrap();
        let recording = recording(TRANSCRIPT, 2_000).unwrap();
        let expected: Vec<Value> = recording
            .lines()
            .map(|line| serde_json::from_str(line.strip_prefix("< ").unwrap()).unwrap())
            .collect();
        let received = dir.join("received.jsonl");
        let (mut client, served) = start(recording, received.clone()).await;
        client.send(hello(PROTOCOL)).await.unwrap();
        let welcome = texts(&mut client, 1).await;
        // A deadline, so a server that never streams fails the test instead of hanging it.
        let streamed =
            tokio::time::timeout(Duration::from_secs(30), texts(&mut client, expected.len()))
                .await
                .unwrap();
        let log = r#"{"id":0,"type":"log","level":"warn","msg":"feed lost"}"#;
        client.send(Frame::text(log)).await.unwrap();
        client.send(Frame::Ping(Vec::new().into())).await.unwrap();
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        assert_eq!(welcome[0]["type"], "welcome");
        assert_eq!(
            welcome[0]["capabilities"],
            json!([
                "run.stream",
                "log.forward",
                "workspace.chats",
                "workspace.chat_commands",
                "diff.view",
                "worktree.manage"
            ])
        );
        assert_eq!(streamed, expected);
        assert_eq!(fs::read_to_string(&received).unwrap(), format!("{log}\n"));
    }

    /// Reads until a status in `state`, with a deadline so a silent server fails the test.
    async fn until_state(client: &mut Client, state: &'static str) -> Vec<Value> {
        let in_state = move |text: &Value| text["entry"]["event"]["status"]["state"] == state;
        tokio::time::timeout(Duration::from_secs(10), until(client, in_state))
            .await
            .unwrap()
    }

    fn kind(text: &Value) -> String {
        let event = &text["entry"]["event"];
        event["update"]["event"]["event"]
            .as_str()
            .or_else(|| event["status"]["state"].as_str())
            .or_else(|| event["kind"].as_str())
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn a_played_line_goes_out_after_what_the_chat_committed_with_it() {
        let chat = Mutex::new(FakeChat::new().unwrap());
        let (replies, mut sent) = unbounded_channel();
        let subscribe = Envelope::from_json(subscription(7, 0).as_bytes()).unwrap();
        // What a subscription missed goes back as its answer, not to `replies`.
        chat.lock().unwrap().answer(&subscribe);
        let played = |event| {
            let run = RunId::try_from("r-1".to_owned()).unwrap();
            let update = Message::Run(RunUpdate { run, event });
            envelope_text(&Envelope {
                id: 0,
                message: update,
            })
            .unwrap()
        };

        let finished = RunEvent::Finished {
            outcome: Outcome::Completed,
        };
        commit_played(&chat, &replies, played(finished));
        let text = "I'll start".to_owned();
        commit_played(&chat, &replies, played(RunEvent::TextDelta { text }));

        let mut order = Vec::new();
        while let Ok(text) = sent.try_recv() {
            let text: Value = serde_json::from_str(&text).unwrap();
            let event = text["event"]["event"]
                .as_str()
                .map_or_else(|| kind(&text), str::to_owned);
            order.push((text["type"].as_str().unwrap().to_owned(), event));
        }
        // The next pass's first delta comes after its run is running again, so it is shown.
        assert_eq!(
            order,
            [
                ("chat", "finished"),
                ("chat", "idle"),
                ("run", "finished"),
                ("chat", "running"),
                ("run", "text_delta")
            ]
            .map(|(kind, event)| (kind.to_owned(), event.to_owned()))
        );
    }

    #[tokio::test]
    async fn a_subscribed_client_hears_every_commit_in_order_under_its_id() {
        let dir = TempDir::new().unwrap();
        let recording = recording(TRANSCRIPT, 1).unwrap();
        let (mut client, served) = start(recording, dir.join("received.jsonl")).await;
        client.send(hello(PROTOCOL)).await.unwrap();
        texts(&mut client, 1).await;
        let allow = ChatAction::Permission {
            answer: PermissionAnswer {
                run: RunId::try_from("r-1".to_owned()).unwrap(),
                prompt: 1,
                allow: true,
            },
        };
        client.send(Frame::text(subscription(7, 0))).await.unwrap();
        let mut seen = until_state(&mut client, "waiting").await;
        client
            .send(Frame::text(command(8, request("q-1"), allow)))
            .await
            .unwrap();
        let answered = until_state(&mut client, "idle").await;
        seen.extend(answered.iter().cloned());
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        let updates: Vec<_> = seen.iter().filter(|text| text["type"] == "chat").collect();
        assert!(updates.iter().all(|update| update["id"] == 7));
        let seqs: Vec<_> = updates
            .iter()
            .map(|update| update["entry"]["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(
            seqs,
            (1..=u64::try_from(seqs.len()).unwrap()).collect::<Vec<_>>()
        );
        assert!(seqs.len() > usize::try_from(SEEDED).unwrap());
        let after: Vec<_> = answered
            .iter()
            .filter(|text| text["type"] == "chat")
            .map(kind)
            .collect();
        assert_eq!(
            after,
            [
                "receipt",
                "permission_answered",
                "running",
                "text",
                "finished",
                "idle"
            ]
        );
    }

    #[tokio::test]
    async fn another_workspaces_chat_and_messages_it_doesnt_know_are_refused() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(PROTOCOL)).await.unwrap();
        texts(&mut client, 1).await;
        let elsewhere = WorkspaceId::try_from("w-2".to_owned()).unwrap();
        let query = Message::ChatQuery(ChatQuery {
            workspace: elsewhere.clone(),
            chat: chat(),
            query: ChatQueryKind::Snapshot,
        });
        let command = Message::ChatCommand(ChatCommand {
            request: request("q-1"),
            workspace: elsewhere,
            chat: chat(),
            action: ChatAction::Interrupt,
            by: None,
        });
        let known = |id, message| serde_json::to_value(Envelope { id, message }).unwrap();
        let sent = [
            known(2, query),
            known(3, command),
            json!({ "id": 4, "type": "chat_query", "workspace": "w-1", "chat": "c-1",
                    "query": { "kind": "search" } }),
            json!({ "id": 5, "type": "chat_command", "request": "q-1", "workspace": "w-1",
                    "chat": "c-1", "action": { "kind": "rewind" } }),
            json!({ "id": 6, "type": "chat.later" }),
        ];
        for text in sent {
            client.send(Frame::text(text.to_string())).await.unwrap();
        }
        // A deadline, so a server that never answers fails the test instead of hanging it.
        let answers = tokio::time::timeout(Duration::from_secs(30), texts(&mut client, 5))
            .await
            .unwrap();
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        let refused: Vec<_> = answers
            .iter()
            .map(|answer| (answer["id"].clone(), answer["code"].clone()))
            .collect();
        assert_eq!(
            refused,
            [
                (json!(2), json!("chat.not_found")),
                (json!(3), json!("chat.not_found")),
                (json!(4), json!("message.unsupported")),
                (json!(5), json!("message.unsupported")),
                (json!(6), json!("message.unsupported")),
            ]
        );
    }

    #[tokio::test]
    async fn a_diff_query_is_answered_one_file_a_frame_under_its_request() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(PROTOCOL)).await.unwrap();
        texts(&mut client, 1).await;
        let query = Message::DiffQuery(DiffQuery {
            request: request("q-1"),
            workspace: workspace(),
            chat: chat(),
            query: DiffQueryKind::Changes,
            by: None,
        });
        let query = envelope_text(&Envelope {
            id: 9,
            message: query,
        })
        .unwrap();
        client.send(Frame::text(query)).await.unwrap();
        let files = usize::try_from(fake_diff::FILES).unwrap();
        // A deadline, so a server that never answers fails the test instead of hanging it.
        let answers = tokio::time::timeout(Duration::from_secs(30), texts(&mut client, files))
            .await
            .unwrap();
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        for answer in answers {
            assert_eq!(
                (&answer["type"], &answer["id"], &answer["request"]),
                (&json!("diff_answer"), &json!(9), &json!("q-1"))
            );
        }
    }

    #[tokio::test]
    async fn worktree_queries_and_commands_are_answered_under_their_ids() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(PROTOCOL)).await.unwrap();
        texts(&mut client, 1).await;
        let state = json!({ "id": 9, "type": "worktree_query", "request": "q-1",
                            "workspace": "w-1", "chat": "c-1", "query": { "kind": "state" } });
        let remove = json!({ "id": 10, "type": "worktree_command", "request": "q-2",
                             "workspace": "w-1", "chat": "c-1", "action": { "kind": "remove" } });
        client.send(Frame::text(state.to_string())).await.unwrap();
        client.send(Frame::text(remove.to_string())).await.unwrap();
        // A deadline, so a server that never answers fails the test instead of hanging it.
        let answers = tokio::time::timeout(Duration::from_secs(30), texts(&mut client, 2))
            .await
            .unwrap();
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        let told: Vec<_> = answers
            .iter()
            .map(|answer| (answer["id"].clone(), answer["answer"]["removed"].clone()))
            .collect();
        assert_eq!(told, [(json!(9), json!(false)), (json!(10), json!(true))]);
    }

    // In memory rather than over TCP, so the paused clock only moves once both sides are idle.
    #[tokio::test(start_paused = true)]
    async fn a_client_can_leave_mid_stream_and_its_close_is_answered() {
        let dir = TempDir::new().unwrap();
        let received = dir.join("received.jsonl");
        let (stream, server_end) = duplex(BUFFER);
        let kept = received.clone();
        let served = tokio::spawn(async move {
            let recording = recording(TRANSCRIPT, 100_000).unwrap();
            serve(server_end, recording, Duration::from_secs(1), &kept).await
        });
        let (mut client, _) = client_async("ws://fake/", stream).await.unwrap();
        client.send(hello(PROTOCOL)).await.unwrap();
        // A deadline, so a server that never streams fails the test instead of hanging it.
        let first = tokio::time::timeout(Duration::from_secs(30), texts(&mut client, 2))
            .await
            .unwrap();
        let log = r#"{"id":0,"type":"log","level":"info","msg":"run shown"}"#;
        client.send(Frame::text(log)).await.unwrap();
        client.close(None).await.unwrap();
        let after: Vec<_> = client.collect().await;

        assert_eq!(first[1]["type"], "run");
        assert!(
            after
                .iter()
                .any(|frame| matches!(frame, Ok(Frame::Close(_)))),
            "the close went unanswered"
        );
        served.await.unwrap().unwrap();
        assert_eq!(fs::read_to_string(&received).unwrap(), format!("{log}\n"));
    }

    fn workspace() -> WorkspaceId {
        WorkspaceId::try_from("w-1".to_owned()).unwrap()
    }

    fn chat() -> ChatId {
        ChatId::try_from("c-1".to_owned()).unwrap()
    }

    fn request(id: &str) -> RequestId {
        RequestId::try_from(id.to_owned()).unwrap()
    }

    fn subscription(id: u32, after: u64) -> String {
        let query = Message::ChatQuery(ChatQuery {
            workspace: workspace(),
            chat: chat(),
            query: ChatQueryKind::Subscribe { after },
        });
        envelope_text(&Envelope { id, message: query }).unwrap()
    }

    fn command(id: u32, request: RequestId, action: ChatAction) -> String {
        let command = Message::ChatCommand(ChatCommand {
            request,
            workspace: workspace(),
            chat: chat(),
            action,
            by: None,
        });
        envelope_text(&Envelope {
            id,
            message: command,
        })
        .unwrap()
    }

    /// The receipts in the chat updates up to the one for `q-2`, each with the id it answers.
    async fn receipts(client: &mut Client) -> Vec<(Value, Value)> {
        let last = |text: &Value| text["entry"]["event"]["receipt"]["request"] == "q-2";
        // A deadline, so a server that never answers fails the test instead of hanging it.
        let seen = tokio::time::timeout(Duration::from_secs(30), until(client, last))
            .await
            .unwrap();
        assert!(seen.iter().all(|text| text["type"] != "receipt"));
        seen.iter()
            .filter(|text| text["entry"]["event"]["kind"] == "receipt")
            .map(|text| {
                (
                    text["id"].clone(),
                    text["entry"]["event"]["receipt"].clone(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn chat_commands_are_kept_and_their_receipts_committed_to_the_chat() {
        let dir = TempDir::new().unwrap();
        let received = dir.join("received.jsonl");
        let (mut client, served) = start(String::new(), received.clone()).await;
        client.send(hello(PROTOCOL)).await.unwrap();
        texts(&mut client, 1).await;
        let interrupt = command(2, request("q-1"), ChatAction::Interrupt);
        let prompt = ChatAction::Prompt {
            text: "hi".to_owned(),
            repo: None,
            machine: None,
            agent: None,
        };
        let prompt = command(3, request("q-2"), prompt);
        let log = r#"{"id":0,"type":"log","level":"info","msg":"sent"}"#;
        for text in [
            subscription(7, SEEDED),
            log.to_owned(),
            interrupt.clone(),
            prompt.clone(),
        ] {
            client.send(Frame::text(text)).await.unwrap();
        }
        let live = receipts(&mut client).await;
        client
            .send(Frame::text(subscription(8, SEEDED)))
            .await
            .unwrap();
        let replayed = receipts(&mut client).await;
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        let accepted = |id: u32| {
            [
                (json!(id), json!({ "request": "q-1", "state": "accepted" })),
                (json!(id), json!({ "request": "q-2", "state": "accepted" })),
            ]
        };
        assert_eq!(live, accepted(7));
        assert_eq!(replayed, accepted(8));
        let sent = [
            subscription(7, SEEDED),
            log.to_owned(),
            interrupt,
            prompt,
            subscription(8, SEEDED),
        ];
        assert_eq!(
            fs::read_to_string(&received).unwrap(),
            sent.map(|text| text + "\n").concat()
        );
    }

    #[tokio::test]
    async fn a_client_that_closes_before_its_hello_is_an_error() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.close(None).await.unwrap();

        assert_eq!(
            served.await.unwrap().unwrap_err().to_string(),
            "the client left before its hello"
        );
    }

    #[tokio::test]
    async fn a_client_on_another_protocol_is_refused() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(PROTOCOL + 1)).await.unwrap();
        let answer = texts(&mut client, 1).await;
        drop(client.close(None).await);

        assert_eq!(answer[0]["code"], "protocol.unsupported");
        assert_eq!(
            served.await.unwrap().unwrap_err().to_string(),
            "the client speaks another protocol"
        );
    }

    #[tokio::test]
    async fn the_first_frame_must_be_a_text_hello() {
        let dir = TempDir::new().unwrap();
        let (mut binary, binary_served) = start(String::new(), dir.join("a")).await;
        binary.send(Frame::binary(vec![1])).await.unwrap();
        let (mut other, other_served) = start(String::new(), dir.join("b")).await;
        other
            .send(Frame::text(
                r#"{"id":1,"type":"error","code":"store.full","retryable":false}"#,
            ))
            .await
            .unwrap();
        let (gone, gone_served) = start(String::new(), dir.join("c")).await;
        drop(gone);

        assert_eq!(
            binary_served.await.unwrap().unwrap_err().to_string(),
            "the client sent a binary frame"
        );
        assert!(
            other_served
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .starts_with("expected a hello")
        );
        assert!(gone_served.await.unwrap().is_err());
    }
}
