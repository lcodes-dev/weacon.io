//! A fake core that answers one client's hello, then replays a recorded run over the socket.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use weacon_protocol::{
    App, Capability, Envelope, Message, RunEvent, RunId, RunUpdate, code, encode_line,
};

use crate::play;

/// A made-up session of one agent run, one `RunEvent` of JSON per line, with whole text blocks.
pub const SESSION: &str = include_str!("../sessions/transcript.jsonl");

// The run every replayed event belongs to.
const RUN: &str = "r-1";

// About four bytes of English per token.
const BYTES_PER_TOKEN: usize = 4;
// Each delta carries about this many bytes, as an agent streams a few tokens at a time.
const DELTA_BYTES: usize = 24;

/// The lines a core sends to stream `session` until at least `tokens` tokens of text have gone
/// out, as a recording for [`crate::replay`]. Each text block is streamed as deltas, then sent
/// whole; the session repeats from the start as often as it takes.
///
/// # Errors
/// When a line of `session` isn't a `RunEvent`, or the session has no text.
pub fn recording(session: &str, tokens: usize) -> Result<String, String> {
    let events = session
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{error}: {line}")))
        .collect::<Result<Vec<RunEvent>, _>>()?;
    let per_pass: usize = events.iter().map(text_tokens).sum();
    if per_pass == 0 {
        return Err("the session has no text".to_owned());
    }
    let run = RunId::try_from(RUN.to_owned())?;
    let mut lines = Vec::new();
    for _ in 0..tokens.div_ceil(per_pass) {
        for event in &events {
            for event in deltas(event).into_iter().chain([event.clone()]) {
                lines.push(line(run.clone(), event)?);
            }
        }
    }
    Ok(lines.join("\n"))
}

/// Answers the hello that `stream`'s client sends, then plays `recording` to it, waiting `pace`
/// after each line.
///
/// # Errors
/// When the first line isn't a hello on this protocol that offers `run.stream`, the connection
/// fails, or the client sends anything more before it hangs up.
pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    recording: String,
    pace: Duration,
) -> Result<(), String> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|error| error.to_string())?;
    let answer = answer(&line, &[])?;
    let welcomed = matches!(answer.message, Message::Welcome(_));
    writer
        .write_all(&encode_line(&answer).map_err(|error| error.to_string())?)
        .await
        .map_err(|error| error.to_string())?;
    if !welcomed {
        return Err("the client speaks another protocol".to_owned());
    }
    play(recording, reader, writer, pace).await
}

/// The answer to the client's first `line`, offering `run.stream` and `more`. A client it
/// welcomes must offer `run.stream`.
pub(crate) fn answer(line: &str, more: &[Capability]) -> Result<Envelope, String> {
    let envelope =
        Envelope::from_json(line.trim().as_bytes()).map_err(|error| error.to_string())?;
    let Message::Hello(hello) = envelope.message else {
        return Err(format!("expected a hello but got {}", line.trim()));
    };
    let core = App {
        name: "weacon-core".to_owned(),
        version: "0.0.0".to_owned(),
        commit: "0000000".to_owned(),
    };
    let stream = Capability::from(code!("run.stream"));
    let capabilities = [std::slice::from_ref(&stream), more].concat();
    let message = hello.answer(core, capabilities);
    if matches!(message, Message::Welcome(_)) && !hello.capabilities.contains(&stream) {
        return Err("the client didn't offer run.stream".to_owned());
    }
    Ok(Envelope {
        id: envelope.id,
        message,
    })
}

/// A fake's command line after its name: `<dir> [tokens] [pace-ms]`, by default 100,000 tokens
/// at one message a millisecond; `None` without a folder or with a number that isn't one.
pub fn args(mut args: impl Iterator<Item = String>) -> Option<(PathBuf, usize, Duration)> {
    let dir = PathBuf::from(args.next()?);
    let tokens = args.next().map_or(Ok(100_000), |arg| arg.parse()).ok()?;
    let pace = args.next().map_or(Ok(1), |arg| arg.parse()).ok()?;
    Some((dir, tokens, Duration::from_millis(pace)))
}

fn text_tokens(event: &RunEvent) -> usize {
    text_of(event).map_or(0, |text| text.len().div_ceil(BYTES_PER_TOKEN))
}

fn text_of(event: &RunEvent) -> Option<&str> {
    if let RunEvent::Text { text } = event {
        Some(text)
    } else {
        None
    }
}

fn deltas(event: &RunEvent) -> Vec<RunEvent> {
    let Some(text) = text_of(event) else {
        return Vec::new();
    };
    let mut deltas = Vec::new();
    let mut piece = String::new();
    for character in text.chars() {
        piece.push(character);
        if piece.len() >= DELTA_BYTES {
            let text = std::mem::take(&mut piece);
            deltas.push(RunEvent::TextDelta { text });
        }
    }
    if !piece.is_empty() {
        deltas.push(RunEvent::TextDelta { text: piece });
    }
    deltas
}

fn line(run: RunId, event: RunEvent) -> Result<String, String> {
    let envelope = Envelope {
        id: 0,
        message: Message::Run(RunUpdate { run, event }),
    };
    let json = envelope.to_json().map_err(|error| error.to_string())?;
    String::from_utf8(json)
        .map(|json| format!("< {json}"))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, duplex};
    use weacon_protocol::{Hello, PROTOCOL};

    use super::*;

    fn hello(id: u32, protocol: u32, capabilities: &[&str]) -> Vec<u8> {
        encode_line(&Envelope {
            id,
            message: Message::Hello(Hello {
                protocol,
                app: App {
                    name: "weacon-extension".to_owned(),
                    version: "0.1.0".to_owned(),
                    commit: "1a2b3c4".to_owned(),
                },
                capabilities: serde_json::from_value(json!(capabilities)).unwrap(),
            }),
        })
        .unwrap()
    }

    fn events(recording: &str) -> Vec<Value> {
        recording
            .lines()
            .map(|line| {
                let envelope: Value =
                    serde_json::from_str(line.strip_prefix("< ").unwrap()).unwrap();
                assert_eq!(envelope["type"], "run");
                assert_eq!(envelope["run"], RUN);
                envelope["event"].clone()
            })
            .collect()
    }

    #[test]
    fn text_streams_as_deltas_then_whole_and_the_session_repeats_until_enough() {
        let text = "é".repeat(13);
        let session = format!(
            "{}\n\n{}\n",
            json!({ "event": "text", "text": text }),
            json!({ "event": "finished", "outcome": { "state": "completed" } })
        );
        let pass = [
            json!({ "event": "text_delta", "text": "é".repeat(12) }),
            json!({ "event": "text_delta", "text": "é" }),
            json!({ "event": "text", "text": text }),
            json!({ "event": "finished", "outcome": { "state": "completed" } }),
        ];

        assert_eq!(events(&recording(&session, 7).unwrap()), pass);
        assert_eq!(
            events(&recording(&session, 8).unwrap()),
            [pass.clone(), pass].concat()
        );
    }

    #[test]
    fn a_session_must_be_json_with_some_text() {
        let finished = json!({ "event": "finished", "outcome": { "state": "completed" } });

        assert_eq!(
            recording(&finished.to_string(), 1),
            Err("the session has no text".to_owned())
        );
        assert!(recording("{", 1).is_err_and(|error| error.ends_with(": {")));
    }

    #[test]
    fn the_recorded_session_has_text_and_tools() {
        let recorded = events(&recording(SESSION, 100_000).unwrap());
        let count = |kind: &str| {
            recorded
                .iter()
                .filter(|event| event["event"] == kind)
                .count()
        };

        assert!(count("text") * 589 >= 100_000);
        assert!(count("tool_call") > 30);
        assert_eq!(count("tool_call"), count("tool_done"));
    }

    #[tokio::test]
    async fn a_hello_is_welcomed_with_its_id_before_the_run_plays() {
        let (mut client, core) = duplex(4096);
        let served = tokio::spawn(serve(core, "< {\"a\":1}".to_owned(), Duration::ZERO));
        client
            .write_all(&hello(7, PROTOCOL, &["run.stream"]))
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        let mut sent = String::new();
        client.read_to_string(&mut sent).await.unwrap();
        let lines: Vec<Value> = sent
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines[0]["id"], 7);
        assert_eq!(lines[0]["type"], "welcome");
        assert_eq!(lines[0]["capabilities"], json!(["run.stream"]));
        assert_eq!(lines[1], json!({ "a": 1 }));
        assert_eq!(served.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn a_client_that_cant_take_run_updates_is_refused() {
        let (mut client, core) = duplex(4096);
        let served = tokio::spawn(serve(core, String::new(), Duration::ZERO));
        client
            .write_all(&hello(1, PROTOCOL, &["chat.send"]))
            .await
            .unwrap();
        client.shutdown().await.unwrap();

        assert_eq!(
            served.await.unwrap(),
            Err("the client didn't offer run.stream".to_owned())
        );
    }

    #[tokio::test]
    async fn anything_but_a_hello_first_is_refused() {
        let (mut client, core) = duplex(4096);
        let served = tokio::spawn(serve(core, String::new(), Duration::ZERO));
        client
            .write_all(
                b"{\"id\":1,\"type\":\"error\",\"code\":\"store.full\",\"retryable\":false}\n",
            )
            .await
            .unwrap();

        assert!(
            served
                .await
                .unwrap()
                .is_err_and(|error| error.starts_with("expected a hello"))
        );
    }

    #[tokio::test]
    async fn another_protocol_is_refused_before_the_run_plays() {
        let (mut client, core) = duplex(4096);
        let served = tokio::spawn(serve(core, "< {\"a\":1}".to_owned(), Duration::ZERO));
        client
            .write_all(&hello(1, PROTOCOL + 1, &[]))
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        let mut sent = String::new();
        client.read_to_string(&mut sent).await.unwrap();
        let reply: Value = serde_json::from_str(sent.trim()).unwrap();

        assert_eq!(reply["code"], "protocol.unsupported");
        assert_eq!(
            served.await.unwrap(),
            Err("the client speaks another protocol".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn each_printed_line_waits_for_the_pace() {
        let (mut client, core) = duplex(4096);
        let start = tokio::time::Instant::now();
        let served = tokio::spawn(serve(
            core,
            "< {\"a\":1}\n< {\"a\":2}".to_owned(),
            Duration::from_millis(5),
        ));
        client
            .write_all(&hello(1, PROTOCOL, &["run.stream"]))
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        client.read_to_string(&mut String::new()).await.unwrap();
        served.await.unwrap().unwrap();

        assert_eq!(start.elapsed(), Duration::from_millis(10));
    }

    #[test]
    fn the_command_line_needs_a_folder_and_has_defaults_for_the_rest() {
        let args = |list: &[&str]| args(list.iter().map(|arg| (*arg).to_owned()));

        assert_eq!(args(&[]), None);
        assert_eq!(
            args(&["dir"]),
            Some((PathBuf::from("dir"), 100_000, Duration::from_millis(1)))
        );
        assert_eq!(
            args(&["dir", "50", "0"]),
            Some((PathBuf::from("dir"), 50, Duration::ZERO))
        );
        assert_eq!(args(&["dir", "5O"]), None);
        assert_eq!(args(&["dir", "50", "slow"]), None);
    }
}
