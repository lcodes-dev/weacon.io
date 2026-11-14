//! The messages Weacon programs exchange, their framing and their error codes.

mod access;
mod capability;
mod chat;
mod code;
mod diff;
mod doc;
mod frame;
mod log;
mod message;
mod pick;
mod receipt;
mod repo;
mod run;
mod task;
mod team;
#[cfg(test)]
mod test_support;
mod token;

pub use access::AccessLevel;
pub use capability::Capability;
pub use chat::{
    ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand, ChatEntry, ChatEvent, ChatId, ChatQuery,
    ChatQueryKind, ChatUpdate,
};
pub use code::Code;
pub use diff::{
    DiffAnswer, DiffAnswerKind, DiffContent, DiffFile, DiffLine, DiffQuery, DiffQueryKind,
    FileChange, Hunk, LineOp,
};
pub use doc::{
    CommitId, DocAnchors, DocContent, DocHistory, DocId, DocList, DocPin, DocRecord, DocRevise,
    DocRevision, DocSummary, links_to_code,
};
pub use frame::{FrameError, LineDecoder, MAX_FRAME, encode_line};
pub use log::{ClientLog, Level, LogEvent};
pub use message::{App, Envelope, ErrorBody, Hello, Message, PROTOCOL, Welcome};
pub use pick::Pick;
pub use receipt::{Outcome, Receipt, RequestId};
pub use repo::RepoId;
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

    use schemars::SchemaGenerator;

    use super::*;

    #[test]
    fn the_committed_schema_is_current() {
        // The HTTP bodies, which no `Envelope` carries, are defined beside it.
        let mut generator = SchemaGenerator::default();
        generator.subschema_for::<SignInStart>();
        generator.subschema_for::<SignInStarted>();
        generator.subschema_for::<SignInFinish>();
        generator.subschema_for::<SignedIn>();
        generator.subschema_for::<AccountSignInStart>();
        generator.subschema_for::<AccountSignInFinish>();
        generator.subschema_for::<AccountSignedIn>();
        generator.subschema_for::<WorkspaceCreate>();
        generator.subschema_for::<WorkspaceSummary>();
        generator.subschema_for::<WorkspaceList>();
        generator.subschema_for::<TaskList>();
        generator.subschema_for::<TaskCreate>();
        generator.subschema_for::<TaskChange>();
        generator.subschema_for::<DocList>();
        generator.subschema_for::<DocRecord>();
        generator.subschema_for::<DocRevise>();
        generator.subschema_for::<DocHistory>();
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
