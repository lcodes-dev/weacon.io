//! An interrupt the engine still owes a chat's turn.

use weacon_protocol::RequestId;

/// An interrupt not yet sent, and the turn it stops.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PendingInterrupt {
    pub(super) request: RequestId,
    pub(super) turn: Option<RequestId>,
    /// Its receipt is in, and its turn hasn't begun: it is sent once the turn does.
    pub(super) committed: bool,
}
