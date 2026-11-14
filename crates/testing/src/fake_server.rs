//! A fake server that answers a web client's hello on a WebSocket, then streams a recorded run
//! to it, one message per text frame, keeps what the client sends, accepts its prompts and
//! answers its diff queries with a made-up change of 40 files.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::{self, Message as Frame};
use weacon_protocol::{Capability, ChatCommand, Envelope, Message, Outcome, Receipt, code};

use crate::fake_chat::FakeChat;
use crate::{BUFFER, fake_core, fake_diff, play};

/// Plays `recording` to the client on `stream`, waiting `pace` after each message, and appends
/// each text frame the client sends after its hello to `received`, one per line. It offers
/// `chat.send` and answers each prompt with a receipt that accepts it, and offers `diff.view`.
///
/// # Errors
/// When the handshake fails, the client's first frame isn't a hello on this protocol that offers
/// `run.stream`, or the connection fails.
pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    recording: String,
    pace: Duration,
    received: &Path,
) -> Result<(), String> {
    let socket = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|error| error.to_string())?;
    let (mut sink, mut frames) = socket.split();
    let hello = next_text(&mut frames)
        .await?
        .ok_or("the client left before its hello")?;
    let offered: [Capability; 4] = [
        code!("log.forward").into(),
        code!("chat.send").into(),
        code!("chat.events").into(),
        code!("diff.view").into(),
    ];
    let answer = fake_core::answer(&hello, &offered)?;
    let welcomed = matches!(answer.message, Message::Welcome(_));
    sink.send(Frame::text(fake_core::text(&answer)?))
        .await
        .map_err(|error| error.to_string())?;
    if !welcomed {
        return Err("the client speaks another protocol".to_owned());
    }
    let chat = Mutex::new(FakeChat::new()?);
    // Sending stops at the first failure, such as the client leaving mid-stream.
    let (played, lines) = duplex(BUFFER);
    let mut lines = BufReader::new(lines).lines();
    let (replies, mut answered) = unbounded_channel();
    let committed = replies.clone();
    let forward = async {
        loop {
            let text = tokio::select! {
                Ok(Some(line)) = lines.next_line() => commit_played(&chat, &committed, line),
                Some(reply) = answered.recv() => reply,
                else => break,
            };
            if sink.send(Frame::text(text)).await.is_err() {
                break;
            }
        }
    };
    let play = play(recording, BufReader::new(tokio::io::empty()), played, pace);
    let sending = async { drop(tokio::join!(play, forward)) };
    let keeping = keep(&mut frames, received, &chat, replies);
    tokio::pin!(keeping);
    tokio::select! {
        kept = &mut keeping => kept,
        () = sending => keeping.await,
    }
}

// Appends each text frame to `received` until the client closes, and hands what answers each
// to `replies`.
async fn keep(
    frames: &mut (impl Stream<Item = tungstenite::Result<Frame>> + Unpin),
    received: &Path,
    chat: &Mutex<FakeChat>,
    replies: UnboundedSender<String>,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(received)
        .await
        .map_err(|error| error.to_string())?;
    while let Some(text) = next_text(frames).await? {
        file.write_all(format!("{text}\n").as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        // Tokio's file writes in the background; a reader may look at any moment.
        file.flush().await.map_err(|error| error.to_string())?;
        reply_to(&text, chat, &replies);
    }
    Ok(())
}

// A played line, to send at once; what tells the client the chat committed it goes to `replies`
// under the chat's lock, so the chat's messages keep the order of their seqs.
fn commit_played(
    chat: &Mutex<FakeChat>,
    replies: &UnboundedSender<String>,
    line: String,
) -> String {
    if let Ok(envelope) = Envelope::from_json(line.as_bytes())
        && let Ok(mut chat) = chat.lock()
    {
        send_all(replies, &chat.played(&envelope));
    }
    line
}

fn send_all(replies: &UnboundedSender<String>, envelopes: &[Envelope]) {
    for text in envelopes
        .iter()
        .filter_map(|envelope| fake_core::text(envelope).ok())
    {
        // This fails only once `serve` is done with the client.
        drop(replies.send(text));
    }
}

// Sends what answers `text` to `replies`: a receipt that accepts a chat command, the answers to a
// diff query, and what the chat says to a query or a command.
fn reply_to(text: &str, chat: &Mutex<FakeChat>, replies: &UnboundedSender<String>) {
    let Ok(envelope) = Envelope::from_json(text.as_bytes()) else {
        return;
    };
    if let Message::ChatCommand(ChatCommand { request, .. }) = &envelope.message {
        let receipt = Message::Receipt(Receipt {
            request: request.clone(),
            outcome: Outcome::Accepted,
        });
        send_all(
            replies,
            &[Envelope {
                id: envelope.id,
                message: receipt,
            }],
        );
    }
    send_all(replies, &fake_diff::answers(&envelope));
    if let Ok(mut chat) = chat.lock() {
        send_all(replies, &chat.answer(&envelope));
    }
}

// The next text frame, skipping pings and pongs; `None` once the connection ends. Reading past
// a close sends its reply, so the client sees a clean close.
async fn next_text(
    frames: &mut (impl Stream<Item = tungstenite::Result<Frame>> + Unpin),
) -> Result<Option<String>, String> {
    while let Some(frame) = frames.next().await {
        match frame.map_err(|error| error.to_string())? {
            Frame::Text(text) => return Ok(Some(text.as_str().to_owned())),
            Frame::Binary(_) => return Err("the client sent a binary frame".to_owned()),
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
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

    use super::*;
    use weacon_protocol::{ChatId, ChatQuery, ChatQueryKind, WorkspaceId};

    use crate::TempDir;
    use crate::fake_chat::SEEDED;
    use crate::fake_core::{SESSION, recording};

    type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

    async fn start(
        recording: String,
        received: PathBuf,
    ) -> (Client, tokio::task::JoinHandle<Result<(), String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve(stream, recording, Duration::ZERO, &received).await
        });
        (connect_async(url).await.unwrap().0, served)
    }

    fn hello(protocol: u32, capabilities: &[&str]) -> Frame {
        Frame::text(
            json!({ "id": 1, "type": "hello", "protocol": protocol, "capabilities": capabilities,
                    "app": { "name": "weacon-web", "version": "0.1.0", "commit": "1a2b3c4" } })
            .to_string(),
        )
    }

    // The texts the client gets, up to the first that `done` holds for.
    async fn until(client: &mut Client, done: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = texts(client, 1).await;
        while !seen.last().is_some_and(&done) {
            seen.extend(texts(client, 1).await);
        }
        seen
    }

    async fn texts(client: &mut Client, count: usize) -> Vec<Value> {
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
        let recording = recording(SESSION, 2_000).unwrap();
        let expected: Vec<Value> = recording
            .lines()
            .map(|line| serde_json::from_str(line.strip_prefix("< ").unwrap()).unwrap())
            .collect();
        let received = dir.join("received.jsonl");
        let (mut client, served) = start(recording, received.clone()).await;
        client.send(hello(1, &["run.stream"])).await.unwrap();
        let welcome = texts(&mut client, 1).await;
        let streamed = texts(&mut client, expected.len()).await;
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
                "chat.send",
                "chat.events",
                "diff.view"
            ])
        );
        assert_eq!(streamed, expected);
        assert_eq!(fs::read_to_string(&received).unwrap(), format!("{log}\n"));
    }

    #[tokio::test]
    async fn a_subscribed_client_hears_the_chat_commit_what_plays_and_what_it_asks() {
        let dir = TempDir::new().unwrap();
        let recording = recording(SESSION, 1).unwrap();
        let (mut client, served) = start(recording, dir.join("received.jsonl")).await;
        client
            .send(hello(1, &["run.stream", "chat.events"]))
            .await
            .unwrap();
        texts(&mut client, 1).await;
        let subscribe = Envelope {
            id: 7,
            message: Message::ChatQuery(ChatQuery {
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                query: ChatQueryKind::Subscribe { after: 0 },
            }),
        };
        let subscribe = fake_core::text(&subscribe).unwrap();
        client.send(Frame::text(subscribe)).await.unwrap();
        let played = |text: &Value| {
            let update = &text["entry"]["event"]["update"];
            update["run"] == "r-1" && text["entry"]["seq"].as_u64() > Some(SEEDED)
        };
        // A deadline, so a server that never tells fails the test instead of hanging it.
        let seen = tokio::time::timeout(Duration::from_secs(10), until(&mut client, played))
            .await
            .unwrap();
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
    }

    #[tokio::test]
    async fn a_diff_query_is_answered_one_file_a_frame_under_its_request() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(1, &["run.stream"])).await.unwrap();
        texts(&mut client, 1).await;
        let query = json!({ "id": 9, "type": "diff_query", "request": "q-1",
                            "workspace": "w-1", "chat": "c-1", "query": { "kind": "changes" } });
        client.send(Frame::text(query.to_string())).await.unwrap();
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

    #[tokio::test(start_paused = true)]
    async fn a_client_can_leave_mid_stream_and_its_close_is_answered() {
        let dir = TempDir::new().unwrap();
        let received = dir.join("received.jsonl");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let kept = received.clone();
        let served = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let recording = recording(SESSION, 100_000).unwrap();
            serve(stream, recording, Duration::from_secs(1), &kept).await
        });
        let (mut client, _) = connect_async(url).await.unwrap();
        client.send(hello(1, &["run.stream"])).await.unwrap();
        let first = texts(&mut client, 2).await;
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
        assert_eq!(served.await.unwrap(), Ok(()));
        assert_eq!(fs::read_to_string(&received).unwrap(), format!("{log}\n"));
    }

    #[tokio::test]
    async fn chat_commands_are_kept_and_accepted_under_their_requests() {
        let dir = TempDir::new().unwrap();
        let received = dir.join("received.jsonl");
        let (mut client, served) = start(String::new(), received.clone()).await;
        client.send(hello(1, &["run.stream"])).await.unwrap();
        texts(&mut client, 1).await;
        let command = |id: u32, request: &str, action: Value| {
            json!({ "id": id, "type": "chat_command", "request": request, "workspace": "w-1",
                    "chat": "c-1", "action": action })
            .to_string()
        };
        let interrupt = command(2, "r-1", json!({ "kind": "interrupt" }));
        let prompt = command(3, "r-2", json!({ "kind": "prompt", "text": "hi" }));
        let log = r#"{"id":0,"type":"log","level":"info","msg":"sent"}"#;
        client.send(Frame::text(log)).await.unwrap();
        client.send(Frame::text(interrupt.clone())).await.unwrap();
        client.send(Frame::text(prompt.clone())).await.unwrap();
        // A deadline, so a server that never answers fails the test instead of hanging it.
        let answers = tokio::time::timeout(Duration::from_secs(30), texts(&mut client, 2))
            .await
            .unwrap();
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        assert_eq!(
            answers,
            [
                json!({ "id": 2, "type": "receipt", "request": "r-1", "state": "accepted" }),
                json!({ "id": 3, "type": "receipt", "request": "r-2", "state": "accepted" })
            ]
        );
        assert_eq!(
            fs::read_to_string(&received).unwrap(),
            format!("{log}\n{interrupt}\n{prompt}\n")
        );
    }

    #[tokio::test]
    async fn a_client_that_closes_before_its_hello_is_an_error() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.close(None).await.unwrap();

        assert_eq!(
            served.await.unwrap(),
            Err("the client left before its hello".to_owned())
        );
    }

    #[tokio::test]
    async fn a_client_on_another_protocol_is_refused() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(99, &["run.stream"])).await.unwrap();
        let answer = texts(&mut client, 1).await;
        drop(client.close(None).await);

        assert_eq!(answer[0]["code"], "protocol.unsupported");
        assert_eq!(
            served.await.unwrap(),
            Err("the client speaks another protocol".to_owned())
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
            binary_served.await.unwrap(),
            Err("the client sent a binary frame".to_owned())
        );
        assert!(
            other_served
                .await
                .unwrap()
                .unwrap_err()
                .starts_with("expected a hello")
        );
        assert!(gone_served.await.unwrap().is_err());
    }
}
