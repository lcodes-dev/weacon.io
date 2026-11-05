use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use weacon_observability::{AsError, Fault, Field, Logger, Reports, Upload};
use weacon_protocol::{Code, code};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Delivers one report; the real one POSTs it, tests record it.
pub(crate) type Deliver = Box<dyn Fn(&Upload) -> Result<(), Fault> + Send + Sync>;

/// Sends error reports when a DSN is set and the person has agreed, and does nothing otherwise.
pub(crate) struct Reporter {
    reports: Option<Reports>,
    deliver: Deliver,
    agreed: AtomicBool,
}

impl Reporter {
    pub(crate) fn new(reports: Option<Reports>, deliver: Deliver) -> Self {
        Self {
            reports,
            deliver,
            agreed: AtomicBool::new(false),
        }
    }

    /// Whether reports can be sent at all, so Settings offers the choice.
    pub(crate) const fn is_available(&self) -> bool {
        self.reports.is_some()
    }

    pub(crate) fn agreed(&self) -> bool {
        self.agreed.load(Ordering::Relaxed)
    }

    pub(crate) fn set_agreed(&self, agreed: bool) {
        self.agreed.store(agreed, Ordering::Relaxed);
    }

    /// Reports `error` under `code`; a report that can't be sent is logged and dropped.
    pub(crate) fn capture_error<E: AsError + ?Sized>(&self, error: &E, code: &Code, log: &Logger) {
        let Some(reports) = self.reports.as_ref().filter(|_| self.agreed()) else {
            return;
        };
        let upload = reports.upload(error, code, random_id(), SystemTime::now());
        if let Err(error) = (self.deliver)(&upload) {
            log.warn("report failed", &[("err", Field::error(&error))]);
        }
    }
}

// Each `RandomState` gets new keys; the first on each thread come from the operating system.
fn random_id() -> u128 {
    let half = || RandomState::new().build_hasher().finish();
    u128::from(half()) << 64 | u128::from(half())
}

/// Sends reports over HTTP.
pub(crate) fn http() -> Deliver {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    Box::new(move |upload| {
        agent
            .post(&upload.url)
            .header("X-Sentry-Auth", &upload.auth)
            .header("Content-Type", "application/x-sentry-envelope")
            .send(upload.body.as_slice())
            .map(drop)
            .map_err(|error| {
                Fault::new(
                    code!("report.send_failed"),
                    "The error report couldn't be sent",
                )
                .because(error)
            })
    })
}
