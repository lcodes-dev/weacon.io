use std::process::ExitCode;

use weacon_observability::{Fault, Field, Logger, Reporter};

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

/// Logs how a command ended, reports a failure, and gives the exit code: 0 when it's done,
/// 1 when a check found something, 2 when it failed.
pub fn finish(log: &Logger, reporter: &Reporter, result: Result<Ending, Fault>) -> ExitCode {
    match result {
        Ok(Ending::Done) => {
            log.info("app stopped", &[]);
            ExitCode::SUCCESS
        }
        Ok(Ending::Findings) => {
            log.info("app stopped", &[("findings", true.into())]);
            ExitCode::from(FINDINGS)
        }
        Err(fault) => {
            log.fatal("app failed", &[("err", Field::error(&fault))]);
            reporter.capture_error(&fault, fault.code(), log);
            ExitCode::from(FAILED)
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
        let fault = Fault::new(code!("store.full"), "The store is full");

        assert_eq!(finish(&log, &reporter, Ok(().into())), ExitCode::SUCCESS);
        assert_eq!(
            finish(&log, &reporter, Ok(Ending::Findings)),
            ExitCode::from(1)
        );
        assert!(uploads.lock().unwrap().is_empty());
        assert_eq!(finish(&log, &reporter, Err(fault)), ExitCode::from(2));
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
