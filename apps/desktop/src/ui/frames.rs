use std::time::{Duration, Instant};

use weacon_observability::Logger;

/// The time between renders while a run streams, to report how smooth it was.
#[derive(Debug, Default)]
pub(crate) struct Frames {
    last: Option<Instant>,
    gaps: Vec<Duration>,
}

impl Frames {
    pub(crate) fn tick(&mut self, now: Instant) {
        if let Some(last) = self.last.replace(now) {
            self.gaps.push(now.saturating_duration_since(last));
        }
    }

    /// The median, 95th percentile and longest gap, in microseconds.
    pub(crate) fn summary(&self) -> Option<[u64; 3]> {
        let mut gaps = self.gaps.clone();
        gaps.sort_unstable();
        let at = |share: usize| {
            let index = (gaps.len() * share / 100).min(gaps.len().saturating_sub(1));
            gaps.get(index)
                .map_or(0, |gap| u64::try_from(gap.as_micros()).unwrap_or(u64::MAX))
        };
        (!gaps.is_empty()).then(|| [at(50), at(95), at(100)])
    }

    pub(crate) fn report(&self, log: &Logger) {
        if let Some([median, p95, longest]) = self.summary() {
            log.info(
                "stream measured",
                &[
                    ("frames", self.gaps.len().into()),
                    ("median_us", median.into()),
                    ("p95_us", p95.into()),
                    ("longest_us", longest.into()),
                ],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_between_renders_are_summarised() {
        let start = Instant::now();
        let mut frames = Frames::default();

        assert_eq!(frames.summary(), None);
        for millis in [0, 10, 30, 31, 41, 141] {
            frames.tick(start + Duration::from_millis(millis));
        }

        assert_eq!(frames.summary(), Some([10_000, 100_000, 100_000]));
    }
}
