//! Which commands in a chat's log a receipt ended, by request id.

use std::collections::HashSet;

use weacon_protocol::{Outcome, Receipt, RequestId};

/// The requests whose commands a receipt other than `accepted` ended.
#[derive(Debug, Default)]
pub(super) struct Requests(HashSet<RequestId>);

impl Requests {
    pub(super) fn receipted(&mut self, receipt: &Receipt) {
        if receipt.outcome != Outcome::Accepted {
            self.0.insert(receipt.request.clone());
        }
    }

    pub(super) fn has_ended(&self, request: &RequestId) -> bool {
        self.0.contains(request)
    }
}
