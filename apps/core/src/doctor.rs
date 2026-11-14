use std::io::{self, Write};
use std::path::Path;

use weacon_agents::{Launch, SignIn, SpawnPolicy};
use weacon_appkit::version_text;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::code;

use crate::app::{App, runtime, state_unavailable};
use crate::workspaces::Workspaces;
use crate::{disk, platform};

/// Prints the build and where this install keeps its state, and fails when it has nowhere to.
pub(crate) fn run(app: &App, out: &mut impl Write) -> Result<(), Fault> {
    let mut print = |text: String| {
        out.write_all(text.as_bytes()).map_err(|error| {
            Fault::new(
                code!("doctor.output_failed"),
                "The report couldn't be printed",
            )
            .because(error)
        })
    };
    print(report(app, platform::free_space))?;
    if let Some(dir) = &app.state_dir {
        let runtime = runtime()?;
        let all = |launches: &[Launch]| runtime.block_on(sign_ins(&app.log, launches));
        print(workspaces(
            &app.log,
            Workspaces::new(dir, app.path.clone()).describe(all),
        ))?;
    }
    if app.log_file.is_none() {
        return Err(state_unavailable());
    }
    Ok(())
}

// Vendor programs asked at once: enough that a slow one doesn't hold up a report, few enough
// for a small machine.
const AT_ONCE: usize = 8;

// A check that fails is logged here, once, and shows as unknown.
async fn sign_ins(log: &Logger, launches: &[Launch]) -> Vec<Option<SignIn>> {
    let failed = |err: Field| log.warn("sign-in check failed", &[("err", err)]);
    let mut answers = Vec::new();
    for together in launches.chunks(AT_ONCE) {
        let checks: Vec<_> = together
            .iter()
            .cloned()
            .map(|launch| tokio::spawn(async move { launch.sign_in().await }))
            .collect();
        for check in checks {
            answers.push(match check.await {
                Ok(Ok(sign_in)) => Some(sign_in),
                Ok(Err(error)) => {
                    failed(Field::error(&error));
                    None
                }
                Err(error) => {
                    failed(Field::error(&error));
                    None
                }
            });
        }
    }
    answers
}

// Workspaces that can't be listed are said to be, never left out.
fn workspaces(log: &Logger, lines: io::Result<String>) -> String {
    lines.unwrap_or_else(|error| {
        log.warn("workspaces unreadable", &[("err", Field::error(&error))]);
        "Workspaces: unavailable\n".to_owned()
    })
}

fn report(app: &App, free_space: disk::FreeSpace) -> String {
    let path = |path: Option<&Path>| {
        path.map_or_else(
            || "unavailable".to_owned(),
            |path| path.display().to_string(),
        )
    };
    let on = app.reporter.is_on() && app.reporter.agreed();
    let reports = if on { "on" } else { "off" };
    let disk = app
        .state_dir
        .as_deref()
        .and_then(|dir| free_space(dir).ok())
        .map_or_else(|| "unavailable".to_owned(), disk::describe);
    format!(
        "weacon {}\nState folder: {}\nLog file: {}\nDisk: {disk}\nError reports: {reports}\n{}",
        version_text(&app.version),
        path(app.state_dir.as_deref()),
        path(app.log_file.as_deref()),
        agent_tools(&app.spawn),
    )
}

fn agent_tools(policy: &SpawnPolicy) -> String {
    let Some(score) = policy.oom_score_adj else {
        return String::new();
    };
    let vars: Vec<_> = policy
        .env
        .iter()
        .map(|(key, value)| format!(" {}={}", key.display(), value.display()))
        .collect();
    format!("Agent tools: OOM score {score}{}\n", vars.concat())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::Path;

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, Broken};

    fn app(state_dir: Option<&str>, log_file: Option<&str>) -> App {
        test_support::app(state_dir.map(Path::new), log_file.map(Path::new))
    }

    #[test]
    fn prints_the_build_and_the_state_folder() {
        let dir = TempDir::new().unwrap();
        let missing = dir.join("missing");
        let state = missing.to_str().unwrap();
        let mut out = Vec::new();
        run(
            &app(Some(state), Some("/srv/weacon/logs/weacon.log")),
            &mut out,
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "weacon 0.1.0 (1a2b3c4)\nState folder: {state}\nLog file: /srv/weacon/logs/weacon.log\nDisk: unavailable\nError reports: off\n"
            )
        );
    }

    #[test]
    fn prints_the_state_folders_free_space() {
        let text = report(&app(Some("/srv"), None), |_| Ok(3 << 30));

        assert!(text.contains("\nDisk: 3072 MiB free (low)\n"));
    }

    #[test]
    fn reports_are_on_only_with_a_dsn_and_consent() {
        let mut app = app(Some("/srv"), None);
        let reports = weacon_observability::Reports::new(
            "https://key@sentry.example.com/1",
            "weacon/0.1.0",
            "weacon-core@0.1.0+1a2b3c4",
            "",
        )
        .ok();
        app.reporter = std::sync::Arc::new(weacon_appkit::reporter(
            reports,
            false,
            Box::new(|_| Ok(())),
        ));
        let without_consent = report(&app, platform::free_space);
        app.reporter.set_agreed(true);

        assert!(
            without_consent.contains("Error reports: off\n"),
            "{without_consent}"
        );
        assert!(report(&app, platform::free_space).contains("Error reports: on\n"));
    }

    #[test]
    fn prints_how_agent_tools_start() {
        let mut app = app(Some("/srv"), Some("/srv/logs/weacon.log"));
        app.spawn = SpawnPolicy {
            env: [
                ("GOMAXPROCS".into(), "2".into()),
                ("CARGO_BUILD_JOBS".into(), "2".into()),
            ]
            .into(),
            oom_score_adj: Some(500),
        };

        assert!(report(&app, platform::free_space).ends_with(
            "Error reports: off\nAgent tools: OOM score 500 CARGO_BUILD_JOBS=2 GOMAXPROCS=2\n"
        ));
    }

    // `true` and `false` stand in for a vendor's status command.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_sign_in_is_checked_in_order_and_a_failed_check_is_logged_once_and_unknown() {
        use weacon_protocol::AgentKind;

        let dir = TempDir::new().unwrap();
        let (log, records) = test_support::recording_logger();
        let launch = |program: &str| Launch {
            agent: AgentKind::Codex,
            program: program.into(),
            config_dir: dir.to_path_buf(),
            cwd: dir.to_path_buf(),
            access: weacon_protocol::AccessLevel::AskFirst,
        };
        let launches = ["true", "/missing/codex", "false"].map(launch);

        let answers = sign_ins(&log, &launches).await;

        assert_eq!(
            answers,
            [Some(SignIn::SignedIn), None, Some(SignIn::SignedOut)]
        );
        assert_eq!(test_support::messages(&records), ["sign-in check failed"]);
    }

    // The script never exits in time. With time paused, the clock moves only when every check
    // is waiting, so it shows how many waits there were, one after another.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn checks_run_a_few_at_once_so_slow_ones_dont_add_up() {
        use std::time::Duration;

        use weacon_protocol::AgentKind;

        let dir = TempDir::new().unwrap();
        let (log, records) = test_support::recording_logger();
        let program = dir.join("hang");
        // Written from a child shell, so no fork of this process inherits a write handle.
        let script = "printf '#!/bin/sh\\nexec sleep 1000\\n' > \"$1\" && chmod 755 \"$1\"";
        let written = std::process::Command::new("sh")
            .args(["-c", script, "sh"])
            .arg(&program)
            .status()
            .unwrap();
        assert!(written.success());
        let hanging = Launch {
            agent: AgentKind::Claude,
            program,
            config_dir: dir.to_path_buf(),
            cwd: dir.to_path_buf(),
            access: weacon_protocol::AccessLevel::AskFirst,
        };
        let launches = vec![hanging; AT_ONCE + 1];
        let started = tokio::time::Instant::now();

        let answers = sign_ins(&log, &launches).await;

        let waits = started.elapsed().as_secs() / 30;
        assert_eq!(waits, 2);
        assert!(started.elapsed() < Duration::from_secs(61));
        assert_eq!(answers, vec![None; AT_ONCE + 1]);
        assert_eq!(test_support::messages(&records).len(), AT_ONCE + 1);
    }

    #[test]
    fn workspaces_that_cant_be_listed_are_said_to_be() {
        let (log, records) = test_support::recording_logger();
        let listed = workspaces(&log, Ok("Workspace w1: Codex signed in\n".to_owned()));
        let unlisted = workspaces(&log, Err(io::ErrorKind::PermissionDenied.into()));

        assert_eq!(listed, "Workspace w1: Codex signed in\n");
        assert_eq!(unlisted, "Workspaces: unavailable\n");
        assert_eq!(test_support::messages(&records), ["workspaces unreadable"]);
    }

    #[test]
    fn fails_without_a_usable_state_folder() {
        let mut out = Vec::new();
        let error = run(&app(None, None), &mut out).unwrap_err();

        assert_eq!(error.code().as_str(), "state.unavailable");
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("State folder: unavailable\nLog file: unavailable\n")
        );
    }

    #[test]
    fn fails_when_it_cant_print() {
        let error = run(
            &app(Some("/srv"), Some("/srv/logs/weacon.log")),
            &mut Broken,
        )
        .unwrap_err();

        assert_eq!(error.code().as_str(), "doctor.output_failed");
    }
}
