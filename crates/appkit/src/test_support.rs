use std::sync::{Arc, Mutex};

use weacon_observability::{Deliver, Reporter, Reports, Upload};

/// A reporter with consent that keeps what it would send.
pub(crate) fn recording_reporter(reports: Option<Reports>) -> (Reporter, Arc<Mutex<Vec<Upload>>>) {
    let uploads = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&uploads);
    let deliver: Deliver = Box::new(move |upload| {
        seen.lock().unwrap().push(upload.clone());
        Ok(())
    });
    (crate::reporter(reports, true, deliver), uploads)
}
