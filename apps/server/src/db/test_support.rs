use weacon_model::Attribution;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatAction, ChatCommand, ChatEntry, ChatEvent, ChatId, MachineId, MachineName,
    PermissionAnswer, RepoId, RequestId, RunEvent, RunId, RunState, RunStatus, RunUpdate,
};

use super::{Incoming, NewChat, NewRequest, Workspace};
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
    let request = id::<RequestId>(&format!("r-{chat}"));
    let (chat, owner) = (id::<ChatId>(chat), member_id(owner));
    let (machine_id, machine) = (id::<MachineId>("mc-1"), id::<MachineName>("Ada's laptop"));
    let repo = id::<RepoId>("site");
    let new = NewChat {
        id: &chat,
        owner: &owner,
        title,
        repo: Some(&repo),
        machine_id: &machine_id,
        machine: &machine,
        at,
    };
    let asked = command(request.as_str(), chat.as_str(), title);
    let first = NewRequest {
        command: &asked,
        member: &owner,
        at,
    };
    workspace.add_chat(&new, &first).await?;
    workspace.chat_made(&chat, &owner).await
}

pub(crate) fn status(state: RunState) -> ChatEvent {
    let status = RunStatus {
        run: id("run-1"),
        state,
        agent: None,
    };
    ChatEvent::RunStatus { status }
}

/// The prompt `text` to `chat` under `request`, as a member of w1 sent it.
pub(crate) fn command(request: &str, chat: &str, text: &str) -> ChatCommand {
    prompt(request, chat, text, None)
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
            text: text.to_owned(),
            repo: None,
            machine: machine.cloned(),
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
    workspace.append_events(&id(chat), &incoming).await
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
    /// Ends `request` to `chat`, as its receipt would.
    pub(crate) async fn close(&self, chat: &str, request: &str) {
        let mut writer = self.db.writer.acquire().await.unwrap();
        let request = id::<RequestId>(request);
        self.end_request(&mut writer, &id(chat), &request)
            .await
            .unwrap();
    }

    /// Puts every chat of the workspace on `machine`.
    pub(crate) async fn move_chats_to(&self, machine: &MachineId) {
        sqlx::query("UPDATE chats SET machine_id = ? WHERE workspace_id = ?")
            .bind(machine.as_str())
            .bind(self.id.as_str())
            .execute(&self.db.writer)
            .await
            .unwrap();
    }
}
