use weacon_observability::{Reporter, Reports};
use weacon_testing::{Uploads, recording_deliver};

/// A reporter with consent that keeps what it would send.
pub(crate) fn recording_reporter(reports: Option<Reports>) -> (Reporter, Uploads) {
    let (deliver, uploads) = recording_deliver();
    (crate::reporter(reports, true, deliver), uploads)
}
