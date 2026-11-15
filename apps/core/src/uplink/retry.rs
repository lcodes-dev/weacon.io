//! When the uplink dials again after a connection ends.

use std::time::Duration;

use weacon_observability::{Fault, Field, Logger};

use super::Ended;

const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_mins(1);

/// When to dial again: a wait doubling from a second to a minute, each cut by up to a fifth
/// at random so that cores don't all dial at once, and back to a second once a connection was
/// up.
#[derive(Debug, Default)]
pub(super) struct Retry {
    failures: u32,
    /// Whether the last try failed, so a run of failures is logged once.
    failing: bool,
}

impl Retry {
    pub(super) fn after(
        &mut self,
        ended: Result<Ended, Fault>,
        log: &Logger,
        random: u128,
    ) -> Duration {
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
        let fifth = (wait / 5).as_millis().max(1);
        let cut = u64::try_from(random % fifth).unwrap_or(0);
        wait.saturating_sub(Duration::from_millis(cut))
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
    fn the_wait_doubles_to_a_minute_less_a_random_fifth_and_resets_once_up() {
        let (log, records) = recording_logger();
        let mut retry = Retry::default();
        let mut waits: Vec<_> = (0..4)
            .map(|_| retry.after(unreachable(), &log, 0).as_secs())
            .collect();
        waits.extend((0..4).map(|_| retry.after(Ok(Ended::Unwelcome), &log, 0).as_secs()));
        let jittered = retry.after(unreachable(), &log, 11_999);
        let after_up = retry.after(Ok(Ended::Lost), &log, 0);
        let failing_again = retry.after(unreachable(), &log, 1_999);

        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(jittered, Duration::from_millis(48_001));
        assert_eq!(after_up, Duration::from_secs(1));
        assert_eq!(failing_again, Duration::from_millis(1_601));
        assert_eq!(
            messages(&records),
            ["uplink failed", "uplink lost", "uplink failed"]
        );
    }
}
