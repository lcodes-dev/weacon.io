//! `fake-agent <args>`: stands in for Claude Code in the web app's end-to-end tests, which link
//! it as `claude` in a temporary folder of their own. It answers `auth status --json` with a
//! made-up sign-in, and a stream-json session by replaying the recorded ones.

use std::process::ExitCode;

use weacon_testing::fake_agent::answer;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match answer(&args, std::io::stdin().lock(), std::io::stdout().lock()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(_) => ExitCode::FAILURE,
    }
}
