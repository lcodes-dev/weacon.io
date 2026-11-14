//! A fake server that answers a web client's hello on a WebSocket, then streams a recorded run
//! to it, one message per text frame, keeps what the client sends, accepts its prompts and
//! answers its diff queries with a made-up change of 40 files.

use std::path::Path;
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::{self, Message as Frame};
use weacon_protocol::{
    Capability, ChatAction, ChatCommand, Envelope, Message, Outcome, Receipt, code,
};

use crate::Failure;
use crate::recording::envelope_text;
use crate::replay::{BUFFER, play};
use crate::{fake_core, fake_diff};

const OFFERED: &[Capability] = &[
    Capability::new(code!("log.forward")),
    Capability::new(code!("chat.send")),
    Capability::new(code!("diff.view")),
];

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
    let (run_out, run_lines) = duplex(BUFFER);
    let (reply_sender, replies) = unbounded_channel();
    let playing = play(recording, BufReader::new(tokio::io::empty()), run_out, pace);
    let sending = async {
        drop(tokio::join!(
            playing,
            send_all(&mut sink, run_lines, replies)
        ));
    };
    let keeping = keep(&mut frames, received, reply_sender);
    tokio::pin!(keeping);
    tokio::select! {
        kept = &mut keeping => kept,
        () = sending => keeping.await,
    }
}

// Sends each of the run's lines and each reply as a text frame, until both end or a send fails,
// such as when the client leaves mid-stream.
async fn send_all(
    sink: &mut (impl Sink<Frame> + Unpin),
    run_lines: impl AsyncRead + Unpin,
    mut replies: UnboundedReceiver<String>,
) {
    let mut run_lines = BufReader::new(run_lines).lines();
    loop {
        let text = tokio::select! {
            Ok(Some(line)) = run_lines.next_line() => line,
            Some(reply) = replies.recv() => reply,
            else => break,
        };
        if sink.send(Frame::text(text)).await.is_err() {
            break;
        }
    }
}

// Appends each text frame to `received` until the client closes, and hands what answers each
// to `replies`.
async fn keep(
    frames: &mut (impl Stream<Item = tungstenite::Result<Frame>> + Unpin),
    received: &Path,
    replies: UnboundedSender<String>,
) -> Result<(), Failure> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(received)
        .await?;
    while let Some(text) = next_text(frames).await? {
        file.write_all(format!("{text}\n").as_bytes()).await?;
        // Tokio's file writes in the background; a reader may look at any moment.
        file.flush().await?;
        for reply in replies_to(&text) {
            // This fails only once `serve` is done with the client.
            drop(replies.send(reply));
        }
    }
    Ok(())
}

// What answers `text`: a receipt that accepts a prompt, or the answers to a diff query.
fn replies_to(text: &str) -> Vec<String> {
    let Ok(envelope) = Envelope::from_json(text.as_bytes()) else {
        return Vec::new();
    };
    let replies = if let Message::ChatCommand(ChatCommand {
        request,
        action: ChatAction::Prompt { .. },
        ..
    }) = &envelope.message
    {
        vec![Envelope {
            id: envelope.id,
            message: Message::Receipt(Receipt {
                request: request.clone(),
                outcome: Outcome::Accepted,
            }),
        }]
    } else {
        fake_diff::answers(&envelope)
    };
    replies
        .iter()
        .filter_map(|reply| envelope_text(reply).ok())
        .collect()
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
    use weacon_protocol::PROTOCOL;

    use crate::TempDir;
    use crate::fake_core::client_hello;
    use crate::recording::{SESSION, recording};

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
        let recording = recording(SESSION, 2_000).unwrap();
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
            json!(["run.stream", "log.forward", "chat.send", "diff.view"])
        );
        assert_eq!(streamed, expected);
        assert_eq!(fs::read_to_string(&received).unwrap(), format!("{log}\n"));
    }

    #[tokio::test]
    async fn a_diff_query_is_answered_one_file_a_frame_under_its_request() {
        let dir = TempDir::new().unwrap();
        let (mut client, served) = start(String::new(), dir.join("received.jsonl")).await;
        client.send(hello(PROTOCOL)).await.unwrap();
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

    // In memory rather than over TCP, so the paused clock only moves once both sides are idle.
    #[tokio::test(start_paused = true)]
    async fn a_client_can_leave_mid_stream_and_its_close_is_answered() {
        let dir = TempDir::new().unwrap();
        let received = dir.join("received.jsonl");
        let (stream, server_end) = duplex(BUFFER);
        let kept = received.clone();
        let served = tokio::spawn(async move {
            let recording = recording(SESSION, 100_000).unwrap();
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

    #[tokio::test]
    async fn a_prompt_is_kept_and_accepted_under_its_request() {
        let dir = TempDir::new().unwrap();
        let received = dir.join("received.jsonl");
        let (mut client, served) = start(String::new(), received.clone()).await;
        client.send(hello(PROTOCOL)).await.unwrap();
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
        let answers = tokio::time::timeout(Duration::from_secs(30), texts(&mut client, 1))
            .await
            .unwrap();
        client.close(None).await.unwrap();
        served.await.unwrap().unwrap();

        assert_eq!(
            answers,
            [json!({ "id": 3, "type": "receipt", "request": "r-2", "state": "accepted" })]
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
