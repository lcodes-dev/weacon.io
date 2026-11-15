use std::process::ExitCode;

use weacon_observability::{Fault, Field};

use crate::App;

const FINDINGS: u8 = 1;
const FAILED: u8 = 2;

/// How a command that ran to the end came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Done,
    /// A check found something, such as a stale anchor.
    Findings,
}

impl From<()> for Ending {
    fn from((): ()) -> Self {
        Self::Done
    }
}

impl App {
    /// Logs how the app ended, reports a failure, and gives the exit code: 0 when it's done,
    /// 1 when a check found something, 2 when it failed.
    #[must_use]
    pub fn finish(&self, result: Result<Ending, Fault>) -> ExitCode {
        match result {
            Ok(Ending::Done) => {
                self.log.info("app stopped", &[]);
                ExitCode::SUCCESS
            }
            Ok(Ending::Findings) => {
                self.log.info("app stopped", &[("findings", true.into())]);
                ExitCode::from(FINDINGS)
            }
            Err(fault) => {
                self.log
                    .fatal("app failed", &[("err", Field::error(&fault))]);
                self.reporter.capture_error(&fault, fault.code(), &self.log);
                ExitCode::from(FAILED)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use weacon_observability::{Level, Reports};
    use weacon_protocol::code;
    use weacon_testing::{messages, recording_logger};

    use super::*;
    use crate::test_support::recording_reporter;

    #[test]
    fn each_ending_has_its_own_exit_code_and_only_a_failure_is_reported() {
        let (log, records) = recording_logger();
        let reports = Reports::new(
            "https://key@sentry.example.com/1",
            "weacon/0.1.0",
            "weacon-core@0.1.0+1a2b3c4",
            "",
        )
        .ok();
        let (reporter, uploads) = recording_reporter(reports);
        let app = App {
            version: weacon_version::Version {
                name: "weacon-core",
                version: "0.1.0",
                commit: "1a2b3c4",
            },
            log,
            reporter: reporter.into(),
        };
        let fault = Fault::new(code!("store.full"), "The store is full");

        assert_eq!(app.finish(Ok(().into())), ExitCode::SUCCESS);
        assert_eq!(app.finish(Ok(Ending::Findings)), ExitCode::from(1));
        assert!(uploads.lock().unwrap().is_empty());
        assert_eq!(app.finish(Err(fault)), ExitCode::from(2));
        assert_eq!(uploads.lock().unwrap().len(), 1);
        assert_eq!(
            messages(&records),
            ["app stopped", "app stopped", "app failed"]
        );
        let records = records.lock().unwrap();
        assert_eq!(records[1].fields["findings"], true);
        assert_eq!(records[2].level, Level::Fatal);
        assert_eq!(records[2].fields["err"]["code"], "store.full");
    }
}
