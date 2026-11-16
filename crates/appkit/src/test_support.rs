use weacon_observability::Reporter;
use weacon_testing::{Uploads, recording_deliver};
use weacon_version::Version;

pub(crate) const VERSION: Version = Version {
    name: "weacon-core",
    version: "0.1.0",
    commit: "1a2b3c4",
};

/// A reporter for `endpoint` that keeps what it would send.
pub(crate) fn recording_reporter(endpoint: Option<&str>) -> (Reporter, Uploads) {
    let (deliver, uploads) = recording_deliver();
    (crate::reporter(endpoint, &VERSION, deliver), uploads)
}
