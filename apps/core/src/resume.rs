//! `weacon resume`: a chat's agent, carried on by hand in the person's terminal, on the session
//! the chat holds, in the chat's folder, with the workspace's own sign-in.

use std::path::Path;

use weacon_appkit::build;
use weacon_client::Connection;
use weacon_observability::Fault;
use weacon_protocol::{
    Capability, ChatAction, ChatCommand, ChatId, ChatQuery, ChatQueryKind, Message, RequestId,
    RunState, WorkspaceId, code,
};

use crate::app::{App, runtime};
use crate::chats;
use crate::chats::log::ChatLog;
use crate::daemon::socket_path;
use crate::runs::{ChatKey, Runs};
use crate::workspaces::Workspaces;

/// Runs the chat's agent here until the person ends it, the chat held meanwhile.
pub(crate) fn run(app: &App, workspace: &WorkspaceId, chat: &ChatId) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    let key = ChatKey::new(workspace, chat);
    // The engine keeps no receipt for a hold, so its request needs no id of its own.
    let request = RequestId::try_from("r-hold".to_owned()).map_err(|_| not_held())?;
    runtime()?.block_on(async {
        // Held first, so the chat's agent has stopped and no prompt starts one until this ends.
        let mut daemon = hold(app, state_dir, &key, &request).await?;
        let resumed = resume(app, state_dir, &key).await;
        if let Some(daemon) = &mut daemon {
            // Closing lets the hold go too, only later; a daemon gone has nothing to hold.
            let release = held(&key, &request, ChatAction::Release);
            daemon.send(3, release).await.unwrap_or_default();
        }
        resumed
    })
}

/// Holds the chat through the running daemon; none when no daemon runs, so none runs its agent.
pub(crate) async fn hold(
    app: &App,
    state_dir: &Path,
    key: &ChatKey,
    request: &RequestId,
) -> Result<Option<Connection>, Fault> {
    let socket = socket_path(state_dir);
    let (welcome, mut daemon) =
        match Connection::open(&socket, build(&app.version), Vec::new()).await {
            Ok(opened) => opened,
            Err(fault) if fault.code().as_str() == "daemon.not_running" => return Ok(None),
            Err(fault) => return Err(fault),
        };
    if !welcome
        .capabilities
        .contains(&Capability::new(code!("chat.hold")))
    {
        return Err(not_held());
    }
    daemon.send(2, held(key, request, ChatAction::Hold)).await?;
    let snapshot = ChatQuery {
        workspace: key.workspace.clone(),
        chat: key.chat.clone(),
        query: ChatQueryKind::Snapshot,
    };
    daemon.send(3, Message::ChatQuery(snapshot)).await?;
    // Answered in order: an error is the hold's, and the snapshot's answer means it was held.
    loop {
        match daemon.next().await? {
            Some(Ok(Message::Error(_))) => {
                return Err(Fault::new(
                    code!("resume.agent_running"),
                    "The chat's agent is on a turn here; carry on once it ends",
                ));
            }
            Some(Ok(Message::ChatAnswer(_))) => return Ok(Some(daemon)),
            Some(_) => {}
            None => {
                return Err(Fault::new(
                    code!("daemon.no_answer"),
                    "The daemon didn't answer",
                ));
            }
        }
    }
}

pub(crate) fn held(key: &ChatKey, request: &RequestId, action: ChatAction) -> Message {
    Message::ChatCommand(ChatCommand {
        request: request.clone(),
        workspace: key.workspace.clone(),
        chat: key.chat.clone(),
        action,
        by: None,
    })
}

fn not_held() -> Fault {
    Fault::new(
        code!("resume.not_held"),
        "The daemon here can't hold the chat; update it, then carry on",
    )
}

async fn resume(app: &App, state_dir: &Path, key: &ChatKey) -> Result<(), Fault> {
    let runs = Runs::load(state_dir)?;
    // A newer core ran it last, and carries it on.
    runs.refuse_newer(key)?;
    let record = runs.of(key).ok_or_else(|| {
        Fault::new(
            code!("resume.no_chat"),
            "This chat hasn't run on this machine",
        )
    })?;
    let session = record.session.clone().ok_or_else(|| {
        Fault::new(
            code!("resume.no_session"),
            "This chat has no session to carry on yet",
        )
    })?;
    // Two agents on one session would both write it.
    if !matches!(record.state, RunState::Sleeping | RunState::Ended) {
        return Err(Fault::new(
            code!("resume.agent_running"),
            "The chat's agent is still running here; carry on once it sleeps",
        ));
    }
    let (workspace, chat) = (&key.workspace, &key.chat);
    let workspaces = Workspaces::new(state_dir, app.path.clone());
    let root = workspaces.root(workspace);
    let repo = ChatLog::open(&root, chat).await?.repo().cloned();
    let cwd = chats::chat_dir(&workspaces, app.work_root.as_deref(), key, repo.as_ref())?;
    let launch = workspaces.launch(workspace, record.agent, cwd)?;
    let ids = [
        ("workspace_id", workspace.as_str().into()),
        ("chat_id", chat.as_str().into()),
    ];
    app.log.info("resume started", &ids);
    launch.require_sign_in().await?;
    launch.resume_by_hand(&session).await?;
    app.log.info("resume ended", &ids);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use weacon_protocol::{AgentKind, AgentSession};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support;

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn ids() -> (WorkspaceId, ChatId) {
        (
            WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            ChatId::try_from("c-1".to_owned()).unwrap(),
        )
    }

    // The chat's record in the journal, as its last run left it.
    fn chat_left(dir: &Path, state: RunState, session: Option<&str>) {
        let (workspace, chat) = ids();
        let key = ChatKey::new(&workspace, &chat);
        runtime().unwrap().block_on(async {
            let mut runs = Runs::load(dir).unwrap();
            runs.begin(&key, AgentKind::Claude, 100).await.unwrap();
            let session = session.map(|id| AgentSession::try_from(id.to_owned()).unwrap());
            runs.update(&key, |record| {
                record.state = state;
                record.session = session;
            })
            .await
            .unwrap();
        });
    }

    fn resume(dir: &TempDir) -> Result<(), Fault> {
        let mut app = test_support::app(Some(&dir.join("state")), None);
        app.work_root = Some(dir.join("work"));
        app.path = Some(dir.join("bin").into_os_string());
        let (workspace, chat) = ids();
        run(&app, &workspace, &chat)
    }

    fn code_of(result: Result<(), Fault>) -> String {
        result.unwrap_err().code().as_str().to_owned()
    }

    #[test]
    fn a_chat_with_no_session_here_has_nothing_to_carry_on() {
        let dir = TempDir::new().unwrap();
        assert_eq!(code_of(resume(&dir)), "resume.no_chat");
        chat_left(&dir.join("state"), RunState::Sleeping, None);
        assert_eq!(code_of(resume(&dir)), "resume.no_session");
    }

    #[test]
    fn a_chat_a_newer_core_ran_is_left_to_it() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let newer = r#"{"workspace":"w-1","chat":"c-1","agent":"gemini","state":"sleeping"}"#;
        let journal = format!(r#"{{"issued":1,"chats":[{newer}]}}"#);
        std::fs::write(state.join("runs.json"), journal).unwrap();

        assert_eq!(code_of(resume(&dir)), "runs.newer_record");
    }

    #[test]
    fn a_chat_whose_agent_is_alive_isnt_carried_on_beside_it() {
        for state in [
            RunState::Starting,
            RunState::Running,
            RunState::Waiting,
            RunState::Idle,
        ] {
            let dir = TempDir::new().unwrap();
            chat_left(&dir.join("state"), state, Some(SESSION));
            assert_eq!(code_of(resume(&dir)), "resume.agent_running", "{state:?}");
        }
    }

    #[test]
    fn a_chat_in_a_repository_is_carried_on_only_in_its_worktree() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        chat_left(&state, RunState::Sleeping, Some(SESSION));
        let (workspace, chat) = ids();
        let root = Workspaces::new(&state, None).root(&workspace);
        let first = weacon_protocol::ChatEvent::Prompt {
            request: "r-1".parse().unwrap(),
            text: "Go".to_owned(),
            repo: Some("site".parse().unwrap()),
            agent: None,
        };
        runtime().unwrap().block_on(async {
            let mut log = ChatLog::open(&root, &chat).await.unwrap();
            log.append(first).await.unwrap();
        });

        // Its repository is gone from the registry, so it has nowhere to carry on.
        assert_eq!(code_of(resume(&dir)), "repo.unknown");
        assert!(!dir.join("work").exists());
    }

    // A stand-in for Claude Code: signed in from its config folder, or not.
    #[cfg(unix)]
    const SIGNED_IN: &str =
        "[ \"$1\" = auth ] && { printf '{\"authMethod\":\"claude.ai\"}'; exit 0; }";
    #[cfg(unix)]
    const RECORDS: &str = "printf '%s %s %s' \"$*\" \"$PWD\" \"$CLAUDE_CONFIG_DIR\" > \"$0.ran\"\n";

    #[cfg(unix)]
    #[test]
    fn a_chat_whose_agent_isnt_signed_in_from_its_workspace_isnt_carried_on() {
        use weacon_testing::platform::write_program;

        let dir = TempDir::new().unwrap();
        chat_left(&dir.join("state"), RunState::Sleeping, Some(SESSION));
        let script = format!("#!/bin/sh\n[ \"$1\" = auth ] && exit 1\n{RECORDS}");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        write_program(&dir.join("bin/claude"), &script).unwrap();

        assert_eq!(code_of(resume(&dir)), "agent.signed_out");
        assert!(!dir.join("bin/claude.ran").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_sleeping_chat_is_carried_on_in_its_folder_with_the_workspaces_sign_in() {
        use std::fs;

        use weacon_testing::platform::write_program;

        for state in [RunState::Sleeping, RunState::Ended] {
            let dir = TempDir::new().unwrap();
            chat_left(&dir.join("state"), state, Some(SESSION));
            let script = format!("#!/bin/sh\n{SIGNED_IN}\n{RECORDS}");
            fs::create_dir_all(dir.join("bin")).unwrap();
            write_program(&dir.join("bin/claude"), &script).unwrap();
            resume(&dir).unwrap();
            let ran = fs::read_to_string(dir.join("bin/claude.ran")).unwrap();
            let (workspace, chat) = ids();
            let workspace = crate::workspaces::folder_name(&workspace);
            let folder = dir
                .join("work")
                .join(&workspace)
                .join(chats::file_stem(&chat));
            let config = dir.join("state/workspaces").join(workspace).join("claude");
            let expected = format!(
                "--resume {SESSION} {} {}",
                folder.display(),
                config.display()
            );
            assert_eq!(ran, expected, "{state:?}");
        }
    }
}
