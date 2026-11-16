//! The messages Weacon programs exchange, their framing and their error codes.

mod access;
mod account;
mod capability;
mod chat;
mod chats;
mod code;
mod diff;
mod doc;
mod frame;
mod hex;
mod install;
mod log;
mod machine;
mod map;
mod mcp;
mod message;
mod pick;
mod proposal;
mod receipt;
mod release;
mod repo;
mod run;
mod search;
mod secret;
mod sessions;
mod settings;
mod symbols;
mod task;
mod team;
mod terminal;
#[cfg(test)]
mod test_support;
mod token;
mod workspace;
mod worktree;

pub use access::AccessLevel;
pub use account::{
    AccountId, AccountSignInFinish, AccountSignInStart, AccountSignedIn, ChallengeId, SignInFinish,
    SignInStart, SignInStarted, SignedIn,
};
pub use capability::Capability;
pub use chat::{
    ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand, ChatEntry, ChatEvent, ChatId, ChatLeg,
    ChatList, ChatLive, ChatQuery, ChatQueryKind, ChatState, ChatSummary, ChatTask, ChatUpdate,
    Quote, QuotedAgent,
};
pub use chats::{ChatsAnswer, ChatsAnswerKind, ChatsQuery, ChatsQueryKind, LocalChat};
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
pub use hex::to_hex;
pub use install::{InstallAction, InstallCommand};
pub use log::{ClientLog, Level, LogEvent};
pub use machine::{
    MachineId, MachineKey, MachineList, MachineName, MachineOs, MachineShares, MachineSummary,
    PairClaim, PairClaimed, PairConfirm, PairLookup, PairPending, PairStart, PairStarted,
    PairingId, SharedMachine, SharedMachineList,
};
pub use map::{MapAnswer, MapQuery, MapQueryKind};
pub use mcp::{FlowArgs, MapArgs, McpCall, ProposeArgs, SearchArgs, SymbolArgs};
pub use message::{App, Envelope, ErrorBody, Hello, Message, PROTOCOL, Welcome};
pub use pick::{Pick, PickNote};
pub use proposal::{
    AnchorChange, Batched, MAX_PROPOSAL, MergeHunk, PinnedDoc, ProposalAbout, ProposalAction,
    ProposalAnswer, ProposalAnswerKind, ProposalChange, ProposalCommand, ProposalDecision,
    ProposalId, ProposalList, ProposalOutcome, ProposalQuery, ProposalQueryKind, ProposalRecord,
    ProposalState, ProposalVerdict, StaleAnchor,
};
pub use receipt::{Outcome, Receipt, RequestId};
pub use release::{
    BuildTarget, Commit, DownloadUrl, ReleaseBuild, ReleaseManifest, ReleaseVersion, Sha256,
    UpdateSchedule,
};
pub use repo::RepoId;
pub use run::{
    AgentKind, AgentOwner, AgentRef, AgentSession, PermissionAnswer, RunEvent, RunId, RunState,
    RunStatus, RunUpdate, WANTS_MAX, Wants,
};
pub use search::{Mark, SearchAnswer, SearchHit, SearchQuery, Snippet};
pub use secret::Secret;
pub use sessions::{
    OutsideSession, SessionsAnswer, SessionsAnswerKind, SessionsQuery, SessionsQueryKind,
};
pub use settings::{
    AgentSignIn, MachineSettings, MachineSettingsAction, MachineSettingsAnswer,
    MachineSettingsAnswerKind, MachineSettingsCommand, MachineSettingsQuery,
    MachineSettingsQueryKind, SignInState,
};
pub use symbols::{
    Symbol, SymbolKind, SymbolsAnswer, SymbolsAnswerKind, SymbolsQuery, SymbolsQueryKind,
};
pub use task::{
    Author, TaskChange, TaskCreate, TaskEditBody, TaskId, TaskList, TaskRecord, TaskStatus,
};
pub use team::{Invitation, InvitationOffer, MemberAction, MemberCommand};
pub use terminal::{
    Bytes, NOISE_MAX, SEALED_MAX, TERMINAL_NOISE, TerminalAction, TerminalCommand, TerminalEvent,
    TerminalId, TerminalInput, TerminalOutput, TerminalUpdate,
};
pub use workspace::{
    Asker, MemberId, MemberList, MemberSummary, WorkspaceChange, WorkspaceCreate, WorkspaceId,
    WorkspaceList, WorkspaceRole, WorkspaceSummary, WorkspaceUpdate,
};
pub use worktree::{
    WorktreeAction, WorktreeAnswer, WorktreeAnswerKind, WorktreeCommand, WorktreeQuery,
    WorktreeQueryKind,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use schemars::SchemaGenerator;

    use super::*;

    // The HTTP bodies, which no `Envelope` carries; schemars sorts the definitions, so this
    // order is only for readers.
    fn define_http_bodies(generator: &mut SchemaGenerator) {
        generator.subschema_for::<SignInStart>();
        generator.subschema_for::<SignInStarted>();
        generator.subschema_for::<SignInFinish>();
        generator.subschema_for::<SignedIn>();
        generator.subschema_for::<AccountSignInStart>();
        generator.subschema_for::<AccountSignInFinish>();
        generator.subschema_for::<AccountSignedIn>();
        generator.subschema_for::<ChatList>();
        generator.subschema_for::<DocList>();
        generator.subschema_for::<DocRecord>();
        generator.subschema_for::<DocRevise>();
        generator.subschema_for::<DocHistory>();
        generator.subschema_for::<PairStart>();
        generator.subschema_for::<PairStarted>();
        generator.subschema_for::<PairLookup>();
        generator.subschema_for::<PairPending>();
        generator.subschema_for::<PairConfirm>();
        generator.subschema_for::<PairClaim>();
        generator.subschema_for::<PairClaimed>();
        generator.subschema_for::<MachineList>();
        generator.subschema_for::<SharedMachineList>();
        generator.subschema_for::<MemberList>();
        generator.subschema_for::<MemberCommand>();
        generator.subschema_for::<SearchQuery>();
        generator.subschema_for::<SearchAnswer>();
        generator.subschema_for::<ProposalList>();
        generator.subschema_for::<ProposalDecision>();
        generator.subschema_for::<ProposalOutcome>();
        generator.subschema_for::<ReleaseManifest>();
        generator.subschema_for::<TaskList>();
        generator.subschema_for::<TaskCreate>();
        generator.subschema_for::<TaskChange>();
        generator.subschema_for::<TerminalInput>();
        generator.subschema_for::<TerminalOutput>();
        generator.subschema_for::<UpdateSchedule>();
        generator.subschema_for::<WorkspaceCreate>();
        generator.subschema_for::<WorkspaceSummary>();
        generator.subschema_for::<WorkspaceList>();
    }

    #[test]
    fn the_committed_schema_is_current() {
        let mut generator = SchemaGenerator::default();
        define_http_bodies(&mut generator);
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
        assert!(
            !current.contains(r#""null""#),
            "an optional field is left out, never null"
        );
    }

    #[test]
    fn a_value_only_ever_received_is_never_written() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in fs::read_dir(src).unwrap() {
            let path = entry.unwrap().path();
            let text = fs::read_to_string(&path).unwrap();
            for line in text
                .lines()
                .filter(|line| line.trim_start().starts_with("#[serde(other"))
            {
                assert!(
                    line.contains("#[serde(other, skip_serializing)]"),
                    "{}: {line}",
                    path.display()
                );
            }
        }
    }
}
