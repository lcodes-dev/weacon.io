//! The messages Weacon programs exchange, their framing and their error codes.

mod capability;
mod code;
mod frame;
mod message;
mod pick;
mod receipt;
mod run;
mod team;
mod token;

pub use capability::Capability;
pub use code::Code;
pub use frame::{FrameError, LineDecoder, MAX_FRAME, encode_line};
pub use message::{App, Envelope, ErrorBody, Hello, Message, PROTOCOL, Welcome};
pub use pick::Pick;
pub use receipt::{Outcome, Receipt, RequestId};
pub use run::{AgentSession, PermissionAnswer, RunEvent, RunId, RunUpdate};
pub use team::{
    ChallengeId, MemberId, SignInFinish, SignInStart, SignInStarted, SignedIn, WorkspaceChange,
    WorkspaceId, WorkspaceUpdate,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn the_committed_schema_is_current() {
        let current =
            serde_json::to_string_pretty(&schemars::schema_for!(Envelope)).unwrap() + "\n";
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema.json");
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            fs::write(&path, &current).unwrap();
        }

        assert!(
            fs::read_to_string(&path).is_ok_and(|committed| committed == current),
            "schema.json is out of date: run UPDATE_SCHEMA=1 cargo test -p weacon-protocol"
        );
    }
}
