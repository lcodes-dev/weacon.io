//! The messages Weacon programs exchange, their framing and their error codes.

mod access;
mod capability;
mod chat;
mod code;
mod frame;
mod log;
mod message;
mod pick;
mod receipt;
mod run;
mod task;
mod team;
mod token;

pub use access::AccessLevel;
pub use capability::Capability;
pub use chat::{
    ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand, ChatEntry, ChatEvent, ChatId, ChatQuery,
    ChatQueryKind, ChatUpdate,
};
pub use code::Code;
pub use frame::{FrameError, LineDecoder, MAX_FRAME, encode_line};
pub use log::{ClientLog, Level, LogEvent};
pub use message::{App, Envelope, ErrorBody, Hello, Message, PROTOCOL, Welcome};
pub use pick::Pick;
pub use receipt::{Outcome, Receipt, RequestId};
pub use run::{
    AgentKind, AgentSession, PermissionAnswer, RunEvent, RunId, RunState, RunStatus, RunUpdate,
};
pub use task::{
    Author, TaskChange, TaskCreate, TaskEditBody, TaskId, TaskList, TaskRecord, TaskStatus,
};
pub use team::{
    AccountId, AccountSignInFinish, AccountSignInStart, AccountSignedIn, ChallengeId, MemberId,
    SignInFinish, SignInStart, SignInStarted, SignedIn, WorkspaceChange, WorkspaceCreate,
    WorkspaceId, WorkspaceList, WorkspaceRole, WorkspaceSummary, WorkspaceUpdate,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use schemars::{Schema, SchemaGenerator};

    use super::*;

    // The HTTP bodies, which no `Envelope` carries; the schema defines them beside it.
    const BODIES: &[fn(&mut SchemaGenerator) -> Schema] = &[
        SchemaGenerator::subschema_for::<SignInStart>,
        SchemaGenerator::subschema_for::<SignInStarted>,
        SchemaGenerator::subschema_for::<SignInFinish>,
        SchemaGenerator::subschema_for::<SignedIn>,
        SchemaGenerator::subschema_for::<AccountSignInStart>,
        SchemaGenerator::subschema_for::<AccountSignInFinish>,
        SchemaGenerator::subschema_for::<AccountSignedIn>,
        SchemaGenerator::subschema_for::<WorkspaceCreate>,
        SchemaGenerator::subschema_for::<WorkspaceSummary>,
        SchemaGenerator::subschema_for::<WorkspaceList>,
    ];

    #[test]
    fn the_committed_schema_is_current() {
        let mut generator = SchemaGenerator::default();
        for body in BODIES {
            body(&mut generator);
        }
        let schema = generator.into_root_schema_for::<Envelope>();
        let current = serde_json::to_string_pretty(&schema).unwrap() + "\n";
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
