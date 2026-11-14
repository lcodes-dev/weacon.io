//! `fake-core <state-dir> [tokens] [pace-ms]`: serves the made-up session to one client, at the
//! socket a core with that state folder would use, then exits.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use weacon_testing::fake_core::serve;
use weacon_testing::platform::listen;
use weacon_testing::recording::{SESSION, args, recording};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let Some((dir, tokens, pace)) = args(std::env::args().skip(1)) else {
        return ExitCode::from(2);
    };
    match run(&dir, tokens, pace).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

async fn run(dir: &Path, tokens: usize, pace: Duration) -> Result<(), String> {
    let recording = recording(SESSION, tokens)?;
    let listener = listen(&dir.join("weacon.sock")).map_err(|error| error.to_string())?;
    let stream = listener
        .accept()
        .await
        .map_err(|error| error.to_string())?
        .0;
    serve(stream, recording, pace).await
}
