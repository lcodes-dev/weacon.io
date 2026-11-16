use weacon_model::Attribution;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatAction, ChatCommand, ChatEntry, ChatEvent, ChatId, MachineId, MachineName,
    PermissionAnswer, RepoId, RunEvent, RunId, RunState, RunStatus, RunUpdate,
};

use super::{Incoming, NewChat, NewLeg, Workspace};
use crate::test_support::member_id;

/// When the db tests' changes are made, in seconds since the Unix epoch.
pub(crate) const NOW: i64 = 1_762_077_600;

/// A change `member` makes at [`NOW`].
pub(super) fn by(member: &str) -> Attribution {
    Attribution::by(member_id(member), NOW)
}

pub(crate) fn code(fault: &Fault) -> &str {
    fault.code().as_str()
}

/// The id `text` names, of whatever id type the test needs.
pub(crate) fn id<T: TryFrom<String>>(text: &str) -> T {
    T::try_from(text.to_owned()).ok().unwrap()
}

/// Makes chat `chat` of `owner` in `workspace` at `at`, its first prompt sent.
pub(crate) async fn make(
    workspace: &Workspace,
    chat: &str,
    owner: &str,
    at: i64,
) -> Result<(), Fault> {
    make_titled(workspace, chat, owner, "Fix the build", at).await
}

/// [`make`], with the first prompt `title`.
pub(crate) async fn make_titled(
    workspace: &Workspace,
    chat: &str,
    owner: &str,
    title: &str,
    at: i64,
) -> Result<(), Fault> {
    let making = Making {
        chat,
        owner,
        title,
        machine: "mc-1",
        at,
        task: None,
    };
    make_as(workspace, &making).await
}

/// A chat [`make_as`] makes.
pub(crate) struct Making<'a> {
    pub(crate) chat: &'a str,
    pub(crate) owner: &'a str,
    pub(crate) title: &'a str,
    pub(crate) machine: &'a str,
    pub(crate) at: i64,
    pub(crate) task: Option<&'a weacon_protocol::ChatTask>,
}

/// Makes the chat `making` describes, its first prompt sent.
pub(crate) async fn make_as(workspace: &Workspace, making: &Making<'_>) -> Result<(), Fault> {
    begin_as(workspace, making).await?;
    let (chat, owner) = (id(making.chat), member_id(making.owner));
    workspace.chat_made(&chat, &owner).await.map(drop)
}

/// Begins chat `chat` as [`make`] does, its first prompt not yet sent.
pub(crate) async fn begin(
    workspace: &Workspace,
    chat: &str,
    owner: &str,
    at: i64,
) -> Result<(), Fault> {
    let making = Making {
        chat,
        owner,
        title: "Fix the build",
        machine: "mc-1",
        at,
        task: None,
    };
    begin_as(workspace, &making).await
}

/// Begins the chat `making` describes, its first prompt not yet sent.
pub(crate) async fn begin_as(workspace: &Workspace, making: &Making<'_>) -> Result<(), Fault> {
    let Making {
        chat,
        owner,
        title,
        machine,
        at,
        ..
    } = *making;
    let (chat, owner) = (id::<ChatId>(chat), member_id(owner));
    let (machine_id, machine) = (id::<MachineId>(machine), id::<MachineName>("Ada's laptop"));
    let repo = id::<RepoId>("site");
    let new = NewChat {
        id: &chat,
        owner: &owner,
        title,
        leg: Some(NewLeg {
            machine_id: &machine_id,
            machine: &machine,
            agent: weacon_protocol::AgentKind::Claude,
            repo: Some(&repo),
        }),
        at,
        task: making.task,
    };
    // Made on its owner's machine, unpaired again unless it was paired before.
    let paired = pair(workspace, &machine_id, &owner).await?;
    let added = workspace.add_chat(&new).await;
    if paired {
        sqlx::query("DELETE FROM machines WHERE machine_id = ?")
            .bind(machine_id.as_str())
            .execute(&workspace.db.writer)
            .await
            .map_err(super::failed)?;
    }
    added
}

// Pairs `machine` to `owner`'s account, unless it is paired already; whether it wasn't.
async fn pair(
    workspace: &Workspace,
    machine: &MachineId,
    owner: &weacon_protocol::MemberId,
) -> Result<bool, Fault> {
    let paired = sqlx::query(
        "INSERT OR IGNORE INTO machines (machine_id, account_id, name, os, machine_key,
             token_hash, paired_at)
         SELECT ?, account_id, 'Made-up machine', 'linux', 'key', randomblob(16), 0
         FROM members WHERE workspace_id = ? AND member_id = ?",
    )
    .bind(machine.as_str())
    .bind(workspace.id.as_str())
    .bind(owner.as_str())
    .execute(&workspace.db.writer)
    .await
    .map_err(super::failed)?;
    Ok(paired.rows_affected() > 0)
}

pub(crate) fn status(state: RunState) -> ChatEvent {
    let status = RunStatus {
        run: id("run-1"),
        state,
        agent: None,
    };
    ChatEvent::RunStatus { status }
}

/// The prompt `text` to `chat` in w1 under `request`, naming `machine` as a first prompt does.
pub(crate) fn prompt(
    request: &str,
    chat: &str,
    text: &str,
    machine: Option<&MachineId>,
) -> ChatCommand {
    ChatCommand {
        request: id(request),
        workspace: id("w1"),
        chat: id(chat),
        action: ChatAction::Prompt {
            quotes: Vec::new(),
            text: text.to_owned(),
            repo: None,
            machine: machine.cloned(),
            agent: None,
            task: None,
        },
        by: None,
    }
}

pub(crate) fn said(text: &str) -> ChatEvent {
    ChatEvent::Run {
        update: RunUpdate {
            run: id::<RunId>("run-1"),
            event: RunEvent::Text {
                text: text.to_owned(),
            },
        },
    }
}

pub(crate) async fn append(
    workspace: &Workspace,
    chat: &str,
    events: &[(Option<u64>, ChatEvent)],
) -> Result<u64, Fault> {
    let incoming: Vec<_> = events
        .iter()
        .map(|(core_seq, event)| Incoming {
            core_seq: *core_seq,
            event,
            at: NOW,
        })
        .collect();
    let chat = id::<ChatId>(chat);
    // From its owner's machine, when it is there.
    match workspace.chat(&chat).await? {
        Some(found) => workspace.append_as(&chat, &found.owner, &incoming).await,
        None => workspace.append_events(&chat, &incoming).await,
    }
}

/// The last of its owner's machine's events `chat` has.
pub(crate) async fn core_seq(workspace: &Workspace, chat: &str) -> u64 {
    let chat = id::<ChatId>(chat);
    let owner = workspace.chat(&chat).await.unwrap().unwrap().owner;
    let leg = workspace.leg(&chat, &owner).await.unwrap().unwrap();
    leg.core_seq
}

pub(crate) fn seqs(entries: &[ChatEntry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.seq).collect()
}

/// Run run-1's permission prompt `prompt`, asked.
pub(crate) fn asks(prompt: u32) -> ChatEvent {
    let run = id::<RunId>("run-1");
    let event = RunEvent::PermissionAsked {
        run: run.clone(),
        prompt,
        tool: "Write".to_owned(),
        input: serde_json::Value::Null,
    };
    ChatEvent::Run {
        update: RunUpdate { run, event },
    }
}

/// Run run-1's permission prompt `prompt`, allowed.
pub(crate) fn answers(prompt: u32) -> ChatEvent {
    let run = id::<RunId>("run-1");
    let answer = PermissionAnswer {
        run: run.clone(),
        prompt,
        allow: true,
    };
    let event = RunEvent::PermissionAnswered { answer };
    ChatEvent::Run {
        update: RunUpdate { run, event },
    }
}

impl Workspace {
    /// Puts every leg of the workspace's chats on `machine`.
    pub(crate) async fn move_chats_to(&self, machine: &MachineId) {
        sqlx::query("UPDATE chat_legs SET machine_id = ? WHERE workspace_id = ?")
            .bind(machine.as_str())
            .bind(self.id.as_str())
            .execute(&self.db.writer)
            .await
            .unwrap();
    }

    /// Keeps `status`, a run's status as JSON, as `chat`'s latest.
    pub(crate) async fn set_run_status(&self, chat: &ChatId, status: &str) {
        sqlx::query("UPDATE chats SET run_status = ? WHERE workspace_id = ? AND chat_id = ?")
            .bind(status)
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(&self.db.writer)
            .await
            .unwrap();
    }

    /// Has every leg's machine have sent its log's first entry, as one that took a prompt has.
    pub(crate) async fn legs_took_prompts(&self) {
        sqlx::query("UPDATE chat_legs SET core_seq = max(core_seq, 1) WHERE workspace_id = ?")
            .bind(self.id.as_str())
            .execute(&self.db.writer)
            .await
            .unwrap();
    }

    /// Pairs `machine` to the account of `member`, as its own.
    pub(crate) async fn pair_for(&self, member: &str, machine: &str) {
        sqlx::query(
            "INSERT INTO machines (machine_id, account_id, name, os, machine_key, token_hash,
                 paired_at)
             SELECT ?, account_id, 'Made-up machine', 'linux', lower(hex(randomblob(32))),
                 randomblob(16), 0
             FROM members WHERE workspace_id = ? AND member_id = ?",
        )
        .bind(machine)
        .bind(self.id.as_str())
        .bind(member)
        .execute(&self.db.writer)
        .await
        .unwrap();
    }

    /// Makes every chat event this workspace stores fail in the database, as a full disk would.
    pub(crate) async fn break_chat_events(&self) {
        sqlx::query(
            "CREATE TRIGGER broken BEFORE INSERT ON chat_events BEGIN SELECT RAISE(ABORT, 'broken'); END",
        )
        .execute(&self.db.writer)
        .await
        .unwrap();
    }

    /// Leaves the workspace no room for more chat events.
    pub(crate) async fn fill_quota(&self) {
        sqlx::query("UPDATE workspaces SET chat_bytes = 536870912 WHERE workspace_id = ?")
            .bind(self.id.as_str())
            .execute(&self.db.writer)
            .await
            .unwrap();
    }
}
