//! `weacon resume`: a chat's agent, carried on by hand in the person's terminal, on the session
//! the chat holds, in the chat's folder, with the workspace's own sign-in.

use weacon_observability::Fault;
use weacon_protocol::{ChatId, RunState, WorkspaceId, code};

use crate::app::{App, runtime};
use crate::chats;
use crate::runs::{ChatKey, Runs};
use crate::workspaces::Workspaces;

/// Runs the chat's agent here until the person ends it.
pub(crate) fn run(app: &App, workspace: &WorkspaceId, chat: &ChatId) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    let key = ChatKey::new(workspace, chat);
    let runs = Runs::load(state_dir)?;
    let record = runs.of(&key).ok_or_else(|| {
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
    let cwd = chats::work_dir(app.work_root.as_deref(), &key)?;
    let launch =
        Workspaces::new(state_dir, app.path.clone()).launch(workspace, record.agent, cwd)?;
    let ids = [
        ("workspace_id", workspace.as_str().into()),
        ("chat_id", chat.as_str().into()),
    ];
    app.log.info("resume started", &ids);
    runtime()?.block_on(launch.resume_by_hand(&session))?;
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

    #[cfg(unix)]
    #[test]
    fn a_sleeping_chat_is_carried_on_in_its_folder_with_the_workspaces_sign_in() {
        use std::fs;

        use weacon_testing::platform::write_program;

        for state in [RunState::Sleeping, RunState::Ended] {
            let dir = TempDir::new().unwrap();
            chat_left(&dir.join("state"), state, Some(SESSION));
            let script = "#!/bin/sh\nprintf '%s %s %s' \"$*\" \"$PWD\" \"$CLAUDE_CONFIG_DIR\" > \"$0.ran\"\n";
            fs::create_dir_all(dir.join("bin")).unwrap();
            write_program(&dir.join("bin/claude"), script).unwrap();
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
