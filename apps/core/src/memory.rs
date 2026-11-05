//! How much memory the core and the processes it started use, logged for long runs.

use std::io;

use weacon_observability::{Field, Logger};

use crate::platform::{self, Process};

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

/// Logs one sample of this process's tree.
pub(crate) async fn record(log: &Logger) {
    let listed = tokio::task::spawn_blocking(platform::processes)
        .await
        .unwrap_or_else(|error| Err(io::Error::other(error)));
    let processes = match listed {
        Ok(processes) => processes,
        Err(error) => return log.warn("memory not sampled", &[("err", Field::error(&error))]),
    };
    let Some(sample) = sample(&processes, std::process::id()) else {
        // As where /proc belongs to another PID namespace.
        return log.warn("memory not sampled", &[]);
    };
    log.info(
        "memory sampled",
        &[
            ("core_kib", sample.core_kib.into()),
            ("descendants_kib", sample.descendants_kib.into()),
            ("descendants", sample.descendants.into()),
        ],
    );
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
    async fn this_process_is_sampled_where_processes_can_be_listed() {
        let (log, records) = crate::test_support::recording_logger();

        record(&log).await;

        let logged = crate::test_support::messages(&records);
        let expected = if platform::PROCESSES {
            "memory sampled"
        } else {
            "memory not sampled"
        };
        assert_eq!(logged, [expected]);
    }
}
