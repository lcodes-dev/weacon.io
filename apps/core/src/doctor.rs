use std::io::Write;
use std::path::Path;

use weacon_agents::SpawnPolicy;
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::{App, state_unavailable};
use crate::{disk, platform};
use weacon_appkit::version_text;

/// Prints the build and where this install keeps its state, and fails when it has nowhere to.
pub(crate) fn run(app: &App, out: &mut impl Write) -> Result<(), Fault> {
    out.write_all(report(app, platform::free_space).as_bytes())
        .map_err(|error| {
            Fault::new(
                code!("doctor.output_failed"),
                "The report couldn't be printed",
            )
            .because(error)
        })?;
    if app.log_file.is_none() {
        return Err(state_unavailable());
    }
    Ok(())
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
    use crate::test_support;

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
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = run(
            &app(Some("/srv"), Some("/srv/logs/weacon.log")),
            &mut Closed,
        )
        .unwrap_err();

        assert_eq!(error.code().as_str(), "doctor.output_failed");
    }
}
