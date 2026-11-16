use std::io::{self, Write};
use std::path::Path;

use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::code;

use crate::app::{App, runtime, state_unavailable};
use crate::machine::{self, Paired};
use crate::workspaces::{Workspaces, sign_ins};

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
    print(report(app))?;
    if let Some(dir) = &app.state_dir {
        print(pairing(&app.log, machine::read(dir)))?;
        let runtime = runtime()?;
        let all = |places| runtime.block_on(sign_ins(&app.log, places));
        print(or_unavailable(
            &app.log,
            Workspaces::new(dir, app.path.clone()).describe(all),
        ))?;
    }
    if app.log_file.is_none() {
        return Err(state_unavailable());
    }
    Ok(())
}

/// The paired server.
fn pairing(log: &Logger, read: Result<Option<Paired>, Fault>) -> String {
    match read {
        Ok(Some(paired)) => format!("Paired with: {}\n", paired.server),
        Ok(None) => String::new(),
        Err(fault) => {
            log.warn("pairing unreadable", &[("err", Field::error(&fault))]);
            "Paired with: unavailable\n".to_owned()
        }
    }
}

// Workspaces that can't be listed are said to be, never left out.
fn or_unavailable(log: &Logger, lines: io::Result<String>) -> String {
    lines.unwrap_or_else(|error| {
        log.warn("workspaces unreadable", &[("err", Field::error(&error))]);
        "Workspaces: unavailable\n".to_owned()
    })
}

fn report(app: &App) -> String {
    let path = |path: Option<&Path>| {
        path.map_or_else(
            || "unavailable".to_owned(),
            |path| path.display().to_string(),
        )
    };
    let reports = if app.reporter.is_on() { "on" } else { "off" };
    format!(
        "weacon {}\nState folder: {}\nLog file: {}\nError reports: {reports}\n",
        app.version.with_commit(),
        path(app.state_dir.as_deref()),
        path(app.log_file.as_deref()),
    )
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
                "weacon 0.1.0 (1a2b3c4)\nState folder: {state}\nLog file: /srv/weacon/logs/weacon.log\nError reports: off\n"
            )
        );
    }

    #[test]
    fn prints_the_pairings_server_but_never_its_token() {
        let dir = TempDir::new().unwrap();
        let paired = Paired {
            server: "https://weacon.example.com".to_owned(),
            pinned: None,
            machine: weacon_protocol::MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: weacon_protocol::AccountId::try_from("a-1".to_owned()).unwrap(),
            token: "t0ken-marker".to_owned(),
        };
        machine::write(&dir, &paired).unwrap();
        let mut out = Vec::new();

        run(&app(dir.to_str(), Some("/srv/weacon.log")), &mut out).unwrap();

        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("\nPaired with: https://weacon.example.com\n"),
            "{printed}"
        );
        assert!(!printed.contains("t0ken-marker"));
    }

    #[test]
    fn says_nothing_before_pairing_and_when_the_pairing_cant_be_read() {
        let (log, records) = test_support::recording_logger();

        let unreadable = pairing(
            &log,
            Err(Fault::new(code!("machine.unreadable"), "Unreadable")),
        );

        assert_eq!(pairing(&log, Ok(None)), "");
        assert_eq!(unreadable, "Paired with: unavailable\n");
        assert_eq!(test_support::messages(&records), ["pairing unreadable"]);
    }

    #[test]
    fn reports_are_on_only_with_an_endpoint() {
        let mut app = app(Some("/srv"), None);
        let without = report(&app);
        app.base.reporter = std::sync::Arc::new(weacon_appkit::reporter(
            Some("http://localhost:4318"),
            &app.version,
            Box::new(|_| Ok(())),
        ));

        assert!(without.contains("Error reports: off\n"), "{without}");
        assert!(report(&app).contains("Error reports: on\n"));
    }

    #[test]
    fn workspaces_that_cant_be_listed_are_said_to_be() {
        let (log, records) = test_support::recording_logger();
        let listed = or_unavailable(&log, Ok("Workspace w1: Codex signed in\n".to_owned()));
        let unlisted = or_unavailable(&log, Err(io::ErrorKind::PermissionDenied.into()));

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
