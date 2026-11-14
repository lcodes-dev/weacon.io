//! `fake-core <state-dir> [tokens] [pace-ms]`: serves the made-up session to one client, at the
//! socket a core with that state folder would use, then exits.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use weacon_testing::Failure;
use weacon_testing::fake_core::serve;
use weacon_testing::platform::listen;
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
    let listener = listen(&dir.join("weacon.sock"))?;
    let stream = listener.accept().await?.0;
    serve(stream, recording, pace).await
}
