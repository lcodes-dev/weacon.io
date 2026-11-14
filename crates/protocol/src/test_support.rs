//! Values and checks the protocol's tests share.

use serde_json::Value;

use crate::{ChatId, Envelope, FrameError, Message, RequestId, RunId, WorkspaceId};

pub(crate) fn envelope(message: Message) -> Envelope {
    Envelope { id: 4, message }
}

/// Checks that `envelope` is `wire` on the wire, both ways and through a frame.
pub(crate) fn travels(envelope: &Envelope, wire: Value) {
    assert_eq!(serde_json::to_value(envelope).unwrap(), wire);
    assert_eq!(
        Envelope::from_json(&envelope.to_json().unwrap()).unwrap(),
        *envelope
    );
    assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), *envelope);
}

/// Checks that what a newer peer sent as `wire` parses but is never sent on, and returns it.
pub(crate) fn never_sent(wire: Value) -> Envelope {
    let envelope: Envelope = serde_json::from_value(wire).unwrap();
    assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{envelope:?}");
    envelope
}

pub(crate) fn request() -> RequestId {
    "r-1".parse().unwrap()
}

pub(crate) fn workspace() -> WorkspaceId {
    "w-1".parse().unwrap()
}

pub(crate) fn chat() -> ChatId {
    "c-1".parse().unwrap()
}

pub(crate) fn run() -> RunId {
    "run-7".parse().unwrap()
}
