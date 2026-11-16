//! When the uplink dials again after a connection ends.

use std::time::Duration;

use weacon_observability::{Fault, Field, Logger};

use super::Ended;

const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_mins(1);

/// When to dial again: a wait doubling from a second to a minute, and back to a second once a
/// connection was up.
#[derive(Debug, Default)]
pub(super) struct Retry {
    failures: u32,
    /// Whether the last try failed, so a run of failures is logged once.
    failing: bool,
}

impl Retry {
    pub(super) fn after(&mut self, ended: Result<Ended, Fault>, log: &Logger) -> Duration {
        match ended {
            Ok(Ended::Lost) => {
                self.failures = 0;
                self.failing = false;
                log.info("uplink lost", &[]);
            }
            Ok(_) => {}
            Err(fault) => {
                if !self.failing {
                    log.warn("uplink failed", &[("err", Field::error(&fault))]);
                }
                self.failing = true;
            }
        }
        let wait = FIRST_WAIT
            .saturating_mul(1 << self.failures.min(6))
            .min(LONGEST_WAIT);
        self.failures = self.failures.saturating_add(1);
        wait
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::code;

    use super::*;
    use crate::test_support::{messages, recording_logger};

    fn unreachable() -> Result<Ended, Fault> {
        Err(Fault::new(
            code!("server.unreachable"),
            "The server couldn't be reached",
        ))
    }

    #[test]
    fn the_wait_doubles_to_a_minute_and_resets_once_up() {
        let (log, records) = recording_logger();
        let mut retry = Retry::default();
        let mut waits: Vec<_> = (0..4)
            .map(|_| retry.after(unreachable(), &log).as_secs())
            .collect();
        waits.extend((0..4).map(|_| retry.after(Ok(Ended::Unwelcome), &log).as_secs()));
        let after_up = retry.after(Ok(Ended::Lost), &log);
        let failing_again = retry.after(unreachable(), &log);

        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(after_up, Duration::from_secs(1));
        assert_eq!(failing_again, Duration::from_secs(2));
        assert_eq!(
            messages(&records),
            ["uplink failed", "uplink lost", "uplink failed"]
        );
    }
}
