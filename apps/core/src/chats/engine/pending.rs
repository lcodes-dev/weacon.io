//! What the engine still owes a chat after a write that failed or was cut short.

use weacon_protocol::{Outcome, PermissionAnswer, RequestId};

/// An answer to a waiting prompt whose receipt is being written, so a retry delivers it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PendingAnswer {
    pub(super) request: RequestId,
    pub(super) answer: PermissionAnswer,
}

/// The outcome a turn's run reported, when the turn's receipt couldn't be written.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PendingEnd {
    pub(super) turn: RequestId,
    pub(super) outcome: Outcome,
}

/// An interrupt committed and not yet sent, and the turn it stops.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PendingInterrupt {
    pub(super) request: RequestId,
    pub(super) turn: Option<RequestId>,
}
