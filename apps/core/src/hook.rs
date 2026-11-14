//! What agents' hooks print. A hook that can't answer in time prints nothing and succeeds, so
//! it never stops an agent.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use weacon_appkit::Ending;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{MapArgs, McpCall, code};

use crate::app::{App, runtime};
use crate::cli::HookCommand;
use crate::mcp::tools;

/// The most a session-start hook adds to an agent's context.
const SESSION_START_BYTES: u32 = 4000;
const LIMIT: Duration = Duration::from_secs(5);

/// Prints what `command`'s hook adds for the current folder, or nothing.
pub(crate) fn run(app: &App, command: HookCommand, out: &mut impl Write) -> Ending {
    let HookCommand::SessionStart = command;
    let printed = std::env::current_dir()
        .map_err(|error| skipped().because(error))
        .and_then(session_start)
        .and_then(|text| {
            out.write_all(text.as_bytes())
                .map_err(|error| skipped().because(error))
        });
    note(&app.log, printed);
    Ending::Done
}

/// Where to begin in `folder`'s code, or nothing when it has no code map.
fn session_start(folder: PathBuf) -> Result<String, Fault> {
    within(LIMIT, move || {
        let call = McpCall::Map(MapArgs {
            budget: Some(SESSION_START_BYTES),
        });
        tools::map_or_empty(&call, &folder)
    })
}

/// `work`'s answer, or a fault once `limit` passes.
fn within<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
) -> Result<T, Fault> {
    let runtime = runtime()?;
    let answer = runtime
        .block_on(async { tokio::time::timeout(limit, tokio::task::spawn_blocking(work)).await });
    // Work still reading a large folder is left to the exit rather than waited for.
    runtime.shutdown_background();
    let joined = answer
        .map_err(|_| Fault::new(code!("hook.too_slow"), "The hook couldn't answer in time"))?;
    joined.map_err(|error| skipped().because(error))?
}

// Only at info, which a hook's stderr never shows, so a hook that can't answer prints nothing.
fn note(log: &Logger, printed: Result<(), Fault>) {
    if let Err(fault) = printed {
        log.info("hook skipped", &[("err", Field::error(&fault))]);
    }
}

fn skipped() -> Fault {
    Fault::new(code!("hook.failed"), "The hook couldn't answer")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::test_support::{app, messages};

    fn repository(dir: &Path, item: &str) {
        fs::create_dir_all(dir.join("src")).unwrap();
        let manifest = format!("[package]\nname = \"{item}\"\nversion = \"0.1.0\"\n");
        fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        fs::write(dir.join("src/lib.rs"), format!("pub fn {item}() {{}}\n")).unwrap();
    }

    #[test]
    fn a_session_starts_with_the_map_of_its_folder_within_a_fixed_size() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        let text = session_start(workspace).unwrap();

        assert!(!text.is_empty() && text.len() <= 4000, "{}", text.len());
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_folder_without_code_adds_nothing_and_a_missing_one_fails() {
        let dir = TempDir::new().unwrap();

        assert_eq!(session_start(dir.to_path_buf()).unwrap(), "");
        let missing = session_start(dir.join("gone")).unwrap_err();
        assert_eq!(missing.code().as_str(), "map.read_failed");
    }

    #[test]
    fn work_that_takes_too_long_is_left_behind() {
        let (release, wait) = std::sync::mpsc::channel::<()>();

        let late = within(Duration::from_millis(10), move || {
            wait.recv().map_err(|_| skipped())
        });

        assert_eq!(late.unwrap_err().code().as_str(), "hook.too_slow");
        drop(release);
        assert_eq!(within(LIMIT, || Ok(1)).unwrap(), 1);
    }

    #[test]
    fn the_hook_prints_the_current_folders_map_and_always_succeeds() {
        let app = app(None, None);
        let mut out = Vec::new();

        let ending = run(&app, HookCommand::SessionStart, &mut out);

        assert_eq!(ending, Ending::Done);
        assert!(!out.is_empty() && out.len() <= 4000, "{}", out.len());
    }

    #[test]
    fn neither_the_code_nor_its_path_reaches_the_log() {
        let dir = TempDir::new().unwrap();
        let folder = dir.join("marker_folder_3");
        repository(&folder, "marker_item_7");
        let (log, records) = recording_logger();

        let map = session_start(folder.clone());
        note(&log, session_start(folder.join("marker_gone_5")).map(drop));

        assert!(map.unwrap().contains("marker_item_7"));
        assert_eq!(messages(&records), ["hook skipped"]);
        let logged = format!("{:?}", records.lock().unwrap());
        assert!(!logged.contains("marker"), "{logged}");
    }
}
