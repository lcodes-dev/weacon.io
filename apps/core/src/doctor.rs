use std::io::Write;
use std::path::Path;

use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::App;
use crate::cli::version_text;

/// Prints the build and where this install keeps its state, and fails when it has nowhere to.
pub(crate) fn run(app: &App, out: &mut impl Write) -> Result<(), Fault> {
    out.write_all(report(app).as_bytes()).map_err(|error| {
        Fault::new(
            code!("doctor.output_failed"),
            "The report couldn't be printed",
        )
        .because(error)
    })?;
    if app.log_file.is_none() {
        return Err(Fault::new(
            code!("state.unavailable"),
            "The state folder can't be used: set HOME or WEACON_STATE_DIR to a writable folder",
        ));
    }
    Ok(())
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
        version_text(&app.version),
        path(app.state_dir.as_deref()),
        path(app.log_file.as_deref()),
    )
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use super::*;
    use crate::report::Reporter;
    use crate::test_support::{VERSION, recording_logger};

    fn app(state_dir: Option<&str>, log_file: Option<&str>) -> App {
        App {
            version: VERSION,
            log: recording_logger().0,
            reporter: Reporter::new(None, Box::new(|_| Ok(()))),
            state_dir: state_dir.map(PathBuf::from),
            log_file: log_file.map(PathBuf::from),
        }
    }

    #[test]
    fn prints_the_build_and_the_state_folder() {
        let mut out = Vec::new();
        run(
            &app(Some("/srv/weacon"), Some("/srv/weacon/logs/weacon.log")),
            &mut out,
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "weacon 0.1.0 (1a2b3c4)\nState folder: /srv/weacon\nLog file: /srv/weacon/logs/weacon.log\nError reports: off\n"
        );
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
