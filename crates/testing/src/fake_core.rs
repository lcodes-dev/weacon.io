//! A fake core that answers one client's hello, then replays a recorded run over the socket.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use weacon_protocol::{App, Capability, Envelope, Message, code, encode_line};

use crate::replay::play;

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
}
