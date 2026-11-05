//! A fake core that answers one client's hello, then replays a recorded run over the socket.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

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
/// When a line of `session` isn't a JSON object, or the session has no text.
pub fn recording(session: &str, tokens: usize) -> Result<String, String> {
    let events = session
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{error}: {line}")))
        .collect::<Result<Vec<Value>, _>>()?;
    let per_pass: usize = events.iter().map(text_tokens).sum();
    if per_pass == 0 {
        return Err("the session has no text".to_owned());
    }
    let mut lines = Vec::new();
    for _ in 0..tokens.div_ceil(per_pass) {
        for event in &events {
            lines.extend(deltas(event).into_iter().map(|delta| line(&delta)));
            lines.push(line(event));
        }
    }
    Ok(lines.join("\n"))
}

/// Answers the hello that `stream`'s client sends, then plays `recording` to it, waiting `pace`
/// after each line.
///
/// # Errors
/// When the first line isn't a hello that offers `run.stream`, the connection fails, or the
/// client sends anything more before it hangs up.
pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    recording: String,
    pace: Duration,
) -> Result<(), String> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut hello = String::new();
    reader
        .read_line(&mut hello)
        .await
        .map_err(|error| error.to_string())?;
    let hello: Value = serde_json::from_str(&hello).map_err(|error| error.to_string())?;
    if hello.get("type") != Some(&json!("hello")) {
        return Err(format!("expected a hello but got {hello}"));
    }
    let offers = hello.get("capabilities").and_then(Value::as_array);
    if !offers.is_some_and(|offers| offers.contains(&json!("run.stream"))) {
        return Err("the client didn't offer run.stream".to_owned());
    }
    let welcome = json!({
        "id": hello.get("id"),
        "type": "welcome",
        "app": { "name": "weacon-core", "version": "0.0.0", "commit": "0000000" },
        "capabilities": ["run.stream"],
    });
    writer
        .write_all(format!("{welcome}\n").as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    play(recording, reader, writer, pace).await
}

fn text_tokens(event: &Value) -> usize {
    text_of(event).map_or(0, |text| text.len().div_ceil(BYTES_PER_TOKEN))
}

fn text_of(event: &Value) -> Option<&str> {
    (event.get("event")? == "text").then(|| event.get("text")?.as_str())?
}

fn deltas(event: &Value) -> Vec<Value> {
    let Some(text) = text_of(event) else {
        return Vec::new();
    };
    let mut deltas = Vec::new();
    let mut piece = String::new();
    for character in text.chars() {
        piece.push(character);
        if piece.len() >= DELTA_BYTES {
            deltas.push(json!({ "event": "text_delta", "text": std::mem::take(&mut piece) }));
        }
    }
    if !piece.is_empty() {
        deltas.push(json!({ "event": "text_delta", "text": piece }));
    }
    deltas
}

fn line(event: &Value) -> String {
    let envelope = json!({ "id": 0, "type": "run", "run": RUN, "event": event });
    format!("< {envelope}")
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, duplex};

    use super::*;

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
            .write_all(b"{\"id\":7,\"type\":\"hello\",\"capabilities\":[\"run.stream\"]}\n")
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
            .write_all(b"{\"id\":1,\"type\":\"hello\",\"capabilities\":[\"chat.send\"]}\n")
            .await
            .unwrap();

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
            .write_all(b"{\"id\":1,\"type\":\"welcome\"}\n")
            .await
            .unwrap();

        assert!(
            served
                .await
                .unwrap()
                .is_err_and(|error| error.starts_with("expected a hello"))
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
            .write_all(b"{\"id\":1,\"type\":\"hello\",\"capabilities\":[\"run.stream\"]}\n")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        client.read_to_string(&mut String::new()).await.unwrap();
        served.await.unwrap().unwrap();

        assert_eq!(start.elapsed(), Duration::from_millis(10));
    }
}
