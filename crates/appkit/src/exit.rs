use std::process::ExitCode;

use weacon_observability::{Fault, Field};

use crate::App;

const FAILED: u8 = 2;

impl App {
    /// Logs how the app ended, reports a failure, and gives the exit code: 0 when it's done,
    /// 2 when it failed.
    #[must_use]
    pub fn finish(&self, result: Result<(), Fault>) -> ExitCode {
        match result {
            Ok(()) => {
                self.log.info("app stopped", &[]);
                ExitCode::SUCCESS
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
    use weacon_observability::Level;
    use weacon_protocol::code;
    use weacon_testing::{messages, recording_logger};

    use super::*;
    use crate::test_support::{VERSION, recording_reporter};

    #[test]
    fn success_and_failure_have_their_own_exit_codes_and_only_a_failure_is_reported() {
        let (log, records) = recording_logger();
        let (reporter, uploads) = recording_reporter(Some("http://localhost:4318"));
        let app = App {
            version: VERSION,
            log,
            reporter: reporter.into(),
        };
        let fault = Fault::new(code!("store.full"), "The store is full");

        assert_eq!(app.finish(Ok(())), ExitCode::SUCCESS);
        assert!(uploads.lock().unwrap().is_empty());
        assert_eq!(app.finish(Err(fault)), ExitCode::from(2));
        assert_eq!(uploads.lock().unwrap().len(), 1);
        assert_eq!(messages(&records), ["app stopped", "app failed"]);
        let records = records.lock().unwrap();
        assert_eq!(records[1].level, Level::Fatal);
        assert_eq!(records[1].fields["err"]["code"], "store.full");
    }
}
