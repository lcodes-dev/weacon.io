//! How much memory the core and the processes it started use, logged for long runs.

use std::io;

use weacon_observability::{Field, Logger};

use crate::platform::Process;

/// The core's own memory and its descendants', such as agents and their tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Sample {
    core_kib: u64,
    descendants_kib: u64,
    descendants: u64,
}

/// Sums `root`'s memory and every process below it; `None` when `root` isn't listed.
fn sample(processes: &[Process], root: u32) -> Option<Sample> {
    let core = processes.iter().find(|process| process.pid == root)?;
    let mut sample = Sample {
        core_kib: core.rss_kib,
        ..Sample::default()
    };
    let mut below = vec![root];
    while let Some(pid) = below.pop() {
        for child in processes.iter().filter(|process| process.parent == pid) {
            below.push(child.pid);
            sample.descendants_kib = sample.descendants_kib.saturating_add(child.rss_kib);
            sample.descendants = sample.descendants.saturating_add(1);
        }
    }
    Some(sample)
}

/// Lists processes; tests pass a fake.
pub(crate) type Processes = fn() -> io::Result<Vec<Process>>;

/// Samples this process's tree and logs each sample; a failure is logged once until one works.
pub(crate) struct Watch {
    processes: Processes,
    failing: bool,
}

impl Watch {
    pub(crate) const fn new(processes: Processes) -> Self {
        Self {
            processes,
            failing: false,
        }
    }

    /// Lists processes off the async runtime, since `/proc` can be slow to read.
    pub(crate) async fn record(&mut self, log: &Logger) {
        let listed = tokio::task::spawn_blocking(self.processes)
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)));
        // Not found means /proc can't see this process, as in another PID namespace.
        let sampled = listed.and_then(|processes| {
            sample(&processes, std::process::id()).ok_or_else(|| io::ErrorKind::NotFound.into())
        });
        match sampled {
            Ok(sample) => {
                self.failing = false;
                log.info(
                    "memory sampled",
                    &[
                        ("core_kib", sample.core_kib.into()),
                        ("descendants_kib", sample.descendants_kib.into()),
                        ("descendants", sample.descendants.into()),
                    ],
                );
            }
            Err(error) => {
                if !self.failing {
                    log.warn("memory not sampled", &[("err", Field::error(&error))]);
                }
                self.failing = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn process(pid: u32, parent: u32, rss_kib: u64) -> Process {
        Process {
            pid,
            parent,
            rss_kib,
        }
    }

    #[test]
    fn the_core_and_everything_below_it_are_summed() {
        let listed = [
            process(30, 20, 300),
            process(1, 0, 9),
            process(10, 1, 100),
            process(20, 10, 200),
            process(40, 1, 400),
            process(50, 40, 500),
        ];

        assert_eq!(
            sample(&listed, 10),
            Some(Sample {
                core_kib: 100,
                descendants_kib: 500,
                descendants: 2,
            })
        );
        assert_eq!(
            sample(&listed, 30).map(|sample| sample.descendants),
            Some(0)
        );
        assert_eq!(sample(&listed, 99), None);
    }

    #[tokio::test]
    async fn a_failure_is_logged_once_until_a_sample_works() {
        let (log, records) = crate::test_support::recording_logger();
        let mut watch = Watch::new(|| Err(io::ErrorKind::PermissionDenied.into()));

        watch.record(&log).await;
        watch.record(&log).await;
        watch.processes = || {
            Ok(vec![Process {
                pid: std::process::id(),
                parent: 1,
                rss_kib: 64,
            }])
        };
        watch.record(&log).await;
        // A listing without this process, as from another PID namespace.
        watch.processes = || Ok(Vec::new());
        watch.record(&log).await;

        assert_eq!(
            crate::test_support::messages(&records),
            ["memory not sampled", "memory sampled", "memory not sampled"]
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn this_process_is_sampled_from_proc() {
        let (log, records) = crate::test_support::recording_logger();

        Watch::new(crate::platform::processes).record(&log).await;

        assert_eq!(crate::test_support::messages(&records), ["memory sampled"]);
    }
}
