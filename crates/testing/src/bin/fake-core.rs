//! `fake-core <state-dir> [tokens] [pace-ms]`: serves the made-up session to one client, at the
//! socket a core with that state folder would use, then exits.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use weacon_testing::fake_core::{SESSION, recording, serve};
use weacon_testing::platform::listen;

const TOKENS: usize = 100_000;
const PACE_MS: u64 = 1;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next().map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    let tokens = args
        .next()
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(TOKENS);
    let pace = args
        .next()
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(PACE_MS);
    match run(dir, tokens, Duration::from_millis(pace)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

async fn run(dir: PathBuf, tokens: usize, pace: Duration) -> Result<(), String> {
    let recording = recording(SESSION, tokens)?;
    let listener = listen(&dir.join("weacon.sock")).map_err(|error| error.to_string())?;
    let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
    serve(stream, recording, pace).await
}
