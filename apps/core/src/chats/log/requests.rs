//! What a chat's log holds of each command, by its request id.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};

use weacon_protocol::{ChatAction, Outcome, Receipt, RequestId};

/// The commands a log holds, by request.
#[derive(Debug, Default)]
pub(super) struct Requests(HashMap<RequestId, Commanded>);

/// What the log holds of one command.
#[derive(Debug, Default)]
pub(super) struct Commanded {
    /// The place of its prompt, when it was one.
    pub(super) prompt: Option<u64>,
    stage: Stage,
    /// A receipt said it failed.
    failed: bool,
    /// The digest of the command refused for good, while the log stays open.
    refused: Option<u64>,
}

/// How far a command got, as its receipts say; a later receipt never moves it back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    #[default]
    Logged,
    Accepted,
    /// A receipt other than `accepted` ended it.
    Ended,
}

/// Whether a command was seen before under the same request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    Never,
    /// The same prompt, committed but without a receipt yet: it may not have been queued.
    Pending,
    /// The same command again: the caller finishes what its first try began.
    Same,
    /// With other content.
    Other,
}

/// A digest of `action`, to tell a command sent again from another under the same id.
pub(crate) fn digest(action: &ChatAction) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(&serde_json::to_vec(action).unwrap_or_default());
    hasher.finish()
}

impl Requests {
    pub(super) fn get(&self, request: &RequestId) -> Option<&Commanded> {
        self.0.get(request)
    }

    pub(super) fn prompted(&mut self, request: &RequestId, seq: u64) {
        self.0.entry(request.clone()).or_default().prompt = Some(seq);
    }

    pub(super) fn receipted(&mut self, receipt: &Receipt) {
        let commanded = self.0.entry(receipt.request.clone()).or_default();
        let stage = if receipt.outcome == Outcome::Accepted {
            Stage::Accepted
        } else {
            Stage::Ended
        };
        commanded.stage = commanded.stage.max(stage);
        commanded.failed |= matches!(receipt.outcome, Outcome::Failed { .. });
    }

    pub(super) fn refused(&mut self, request: &RequestId, digest: u64) {
        if let Some(commanded) = self.0.get_mut(request) {
            commanded.refused = Some(digest);
        }
    }

    pub(super) fn has_ended(&self, request: &RequestId) -> bool {
        self.0
            .get(request)
            .is_some_and(|commanded| commanded.stage == Stage::Ended)
    }
}

impl Commanded {
    /// How `action` compares with a command that logged no prompt: any other command counts as
    /// the same, and so does anything under a refused request but another refused command.
    pub(super) fn seen_unprompted(&self, action: &ChatAction) -> Seen {
        let same = self.refused.map_or_else(
            || self.failed || !matches!(action, ChatAction::Prompt { .. }),
            |refused| refused == digest(action),
        );
        if same { Seen::Same } else { Seen::Other }
    }

    /// How a prompt compares with this one's logged prompt, `same` when its content matches.
    pub(super) const fn seen_prompt(&self, same: bool) -> Seen {
        match (same, self.stage) {
            (true, Stage::Logged) => Seen::Pending,
            (true, Stage::Accepted | Stage::Ended) => Seen::Same,
            (false, _) => Seen::Other,
        }
    }
}
