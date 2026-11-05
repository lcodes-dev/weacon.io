//! Test helpers for Weacon's crates.

pub mod fake_core;
pub mod platform;

use std::time::Duration;

use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    DuplexStream, duplex,
};
use tokio::task::JoinHandle;

const BUFFER: usize = 1 << 16;

/// A fake agent process that replays a recorded session.
#[derive(Debug)]
pub struct Replay {
    /// The fake's stdin, for the driver to write to.
    pub stdin: DuplexStream,
    /// The fake's stdout, for the driver to read.
    pub stdout: BufReader<DuplexStream>,
    /// Ends once the recording has played and the driver has closed `stdin`, with the first
    /// way the driver strayed from the recording.
    pub done: JoinHandle<Result<(), String>>,
}

/// Replays `recording` on the current tokio runtime. A line starting `< ` is one the agent
/// prints; a line starting `> ` is one the driver must send, compared as JSON. After the last
/// line the fake hangs up, as an agent that exits, and anything the driver still sends is
/// a mismatch too.
#[must_use]
pub fn replay(recording: &str) -> Replay {
    let (stdin, fake_in) = duplex(BUFFER);
    let (fake_out, stdout) = duplex(BUFFER);
    Replay {
        stdin,
        stdout: BufReader::new(stdout),
        done: tokio::spawn(play(
            recording.to_owned(),
            BufReader::new(fake_in),
            fake_out,
            Duration::ZERO,
        )),
    }
}

// Plays `recording` as `replay` describes, waiting `pace` after each line it prints.
async fn play(
    recording: String,
    mut input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    pace: Duration,
) -> Result<(), String> {
    for step in recording.lines().filter(|line| !line.trim().is_empty()) {
        if let Some(line) = step.strip_prefix("< ") {
            output
                .write_all(format!("{line}\n").as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            if !pace.is_zero() {
                tokio::time::sleep(pace).await;
            }
        } else if let Some(expected) = step.strip_prefix("> ") {
            let mut sent = String::new();
            input
                .read_line(&mut sent)
                .await
                .map_err(|error| error.to_string())?;
            if serde_json::from_str::<Value>(&sent).ok() != Some(json(expected)?) {
                return Err(format!("expected {expected}\nbut got {sent}"));
            }
        } else {
            return Err(format!("not a recorded line: {step}"));
        }
    }
    output.shutdown().await.map_err(|error| error.to_string())?;
    let mut rest = String::new();
    input
        .read_to_string(&mut rest)
        .await
        .map_err(|error| error.to_string())?;
    if rest.is_empty() {
        Ok(())
    } else {
        Err(format!("sent after the agent exited: {rest}"))
    }
}

fn json(line: &str) -> Result<Value, String> {
    serde_json::from_str(line).map_err(|error| format!("{error} in the recording: {line}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(recording: &str, sent: &[&str]) -> (Vec<String>, Result<(), String>) {
        let mut fake = replay(recording);
        for line in sent {
            fake.stdin.write_all(line.as_bytes()).await.unwrap();
        }
        drop(fake.stdin);
        let mut printed = Vec::new();
        let mut lines = fake.stdout.lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            printed.push(line);
        }
        (printed, fake.done.await.unwrap())
    }

    #[tokio::test]
    async fn plays_the_agents_lines_and_checks_the_drivers() {
        let recording = "< {\"a\":1}\n\n> {\"b\":2, \"c\":[3]}\n< {\"d\":4}\n";

        assert_eq!(
            run(recording, &["{\"c\":[3],\"b\":2}\n"]).await,
            (vec!["{\"a\":1}".to_owned(), "{\"d\":4}".to_owned()], Ok(()))
        );
    }

    #[tokio::test]
    async fn a_different_line_is_a_mismatch() {
        let (_, result) = run("> {\"b\":2}\n< {\"d\":4}", &["{\"b\":3}\n"]).await;

        assert_eq!(
            result,
            Err("expected {\"b\":2}\nbut got {\"b\":3}\n".to_owned())
        );
    }

    #[tokio::test]
    async fn sending_after_the_end_or_nothing_at_all_is_a_mismatch() {
        let (_, extra) = run("< {\"a\":1}", &["{\"b\":2}\n"]).await;
        let (_, nothing) = run("> {\"b\":2}", &[]).await;

        assert_eq!(
            extra,
            Err("sent after the agent exited: {\"b\":2}\n".to_owned())
        );
        assert_eq!(nothing, Err("expected {\"b\":2}\nbut got ".to_owned()));
    }

    #[tokio::test]
    async fn a_broken_recording_says_where() {
        let (_, unmarked) = run("{\"a\":1}", &[]).await;
        let (_, bad_json) = run("> {", &["{}\n"]).await;

        assert_eq!(unmarked, Err("not a recorded line: {\"a\":1}".to_owned()));
        assert!(bad_json.is_err_and(|error| error.ends_with("in the recording: {")));
    }
}
