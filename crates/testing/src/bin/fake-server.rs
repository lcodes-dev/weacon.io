//! `fake-server <dir> [tokens] [pace-ms]`: serves the made-up session over a WebSocket on a free
//! port of 127.0.0.1, to each web client that connects, until stopped. It prints its address on
//! one line once it listens, and appends what clients send to `<dir>/received.jsonl`, which it
//! empties first.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use weacon_testing::Failure;
use weacon_testing::fake_server::serve;
use weacon_testing::recording::{FakeArgs, SESSION, args, recording};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let Some(FakeArgs { dir, tokens, pace }) = args(std::env::args().skip(1)) else {
        return ExitCode::from(2);
    };
    match run(&dir, tokens, pace).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

async fn run(dir: &Path, tokens: usize, pace: Duration) -> Result<(), Failure> {
    let recording = recording(SESSION, tokens)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let received = dir.join("received.jsonl");
    tokio::fs::write(&received, "").await?;
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("ws://{address}/\n").as_bytes())
        .await?;
    stdout.flush().await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let (recording, received) = (recording.clone(), received.clone());
        tokio::spawn(async move { serve(stream, recording, pace, &received).await });
    }
}
