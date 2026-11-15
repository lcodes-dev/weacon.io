//! Answers a person's questions about the sessions they ran outside Weacon, read from their
//! own agent folders and never stored.

use std::path::{Path, PathBuf};

use weacon_agents::outside;
use weacon_agents::sessions::Disk;
use weacon_observability::Logger;
use weacon_protocol::{AgentKind, Envelope, Message, SessionsAnswer, SessionsQuery, code};

use crate::session::Reply;

#[derive(Clone, Debug, Default)]
pub(crate) struct Outside {
    homes: Vec<(AgentKind, PathBuf)>,
}

impl Outside {
    /// The person's own agent folders, less any in or around `state_dir`, where each
    /// workspace's sign-ins are kept.
    pub(crate) fn new(mut homes: Vec<(AgentKind, PathBuf)>, state_dir: &Path) -> Self {
        homes.retain(|(_, dir)| !dir.starts_with(state_dir) && !state_dir.starts_with(dir));
        Self { homes }
    }

    /// The answer to `query`, under its request; `None` for a query this build doesn't know.
    pub(crate) async fn answer(&self, query: SessionsQuery) -> Option<SessionsAnswer> {
        let homes = self.homes.clone();
        let kind = query.query;
        let answer = tokio::task::spawn_blocking(move || {
            let dirs: Vec<_> = homes
                .iter()
                .map(|(agent, dir)| (*agent, dir.as_path()))
                .collect();
            outside::answer(&Disk, &dirs, &kind)
        })
        .await
        .ok()??;
        Some(SessionsAnswer {
            request: query.request,
            machine: query.machine,
            answer,
        })
    }

    /// The reply to `query` in envelope `id`. Only the daemon socket's owner, or the machine's
    /// owner through the uplink, reaches this.
    pub(crate) async fn reply(&self, id: u32, query: SessionsQuery, log: &Logger) -> Reply {
        let request = query.request.as_str().into();
        let Some(answer) = self.answer(query).await else {
            return Reply::error(id, code!("message.unsupported"), false);
        };
        log.info("sessions answered", &[("request_id", request)]);
        Reply {
            envelope: Some(Envelope {
                id,
                message: Message::SessionsAnswer(answer),
            }),
            close: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use weacon_protocol::{
        AgentSession, ChatEvent, MachineId, RequestId, SessionsAnswerKind, SessionsQueryKind,
    };
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::test_support::error_code;

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn query(kind: SessionsQueryKind) -> SessionsQuery {
        SessionsQuery {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            query: kind,
            by: None,
        }
    }

    fn open() -> SessionsQueryKind {
        SessionsQueryKind::Open {
            agent: AgentKind::Claude,
            session: AgentSession::try_from(SESSION.to_owned()).unwrap(),
            after: 0,
        }
    }

    // A person's own Claude Code folder holding one session that ran in `folder`.
    fn claude_home(dir: &Path, folder: &str, prompt: &str) -> PathBuf {
        let home = dir.join("claude");
        let project = home.join("projects").join("-work");
        fs::create_dir_all(&project).unwrap();
        let line = json!({ "type": "user", "cwd": folder, "message": { "content": prompt } });
        fs::write(
            project.join(format!("{SESSION}.jsonl")),
            format!("{line}\n"),
        )
        .unwrap();
        home
    }

    fn answer(reply: Reply) -> SessionsAnswer {
        match reply.envelope.map(|envelope| envelope.message) {
            Some(Message::SessionsAnswer(answer)) => answer,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn folders_in_or_around_the_state_folder_are_never_read() {
        let state = PathBuf::from("/home/ada/.local/state/weacon");
        let homes = vec![
            (AgentKind::Claude, PathBuf::from("/home/ada/.claude")),
            (AgentKind::Codex, state.join("w-1").join("codex")),
            (AgentKind::Claude, PathBuf::from("/home/ada")),
        ];

        let outside = Outside::new(homes, &state);

        assert_eq!(
            outside.homes,
            [(AgentKind::Claude, PathBuf::from("/home/ada/.claude"))]
        );
    }

    #[tokio::test]
    async fn the_owner_sees_the_list_and_a_session_and_no_content_reaches_the_log() {
        let dir = TempDir::new().unwrap();
        let home = claude_home(&dir, "/MARKER-FOLDER", "MARKER-PROMPT");
        let outside = Outside::new(vec![(AgentKind::Claude, home)], &dir.join("state"));
        let (log, records) = recording_logger();

        let listed = answer(outside.reply(7, query(SessionsQueryKind::List), &log).await);
        let opened = answer(outside.reply(8, query(open()), &log).await);

        assert_eq!(
            (listed.request.as_str(), listed.machine.as_str()),
            ("r-1", "mc-1")
        );
        let SessionsAnswerKind::List { sessions } = listed.answer else {
            panic!("{:?}", listed.answer);
        };
        assert_eq!(
            sessions.first().unwrap().folder.as_deref(),
            Some("/MARKER-FOLDER")
        );
        let SessionsAnswerKind::Opened {
            events,
            more: false,
        } = opened.answer
        else {
            panic!("{:?}", opened.answer);
        };
        assert!(matches!(&events.first().unwrap().event,
            ChatEvent::OutsidePrompt { text } if text == "MARKER-PROMPT"));
        let logged = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(
            logged.contains("sessions answered") && logged.contains("r-1"),
            "{logged}"
        );
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_query_this_build_doesnt_know_is_refused() {
        let (log, records) = recording_logger();

        let reply = Outside::default()
            .reply(3, query(SessionsQueryKind::Newer), &log)
            .await;

        let envelope = reply.envelope.unwrap();
        assert_eq!(envelope.id, 3);
        assert_eq!(error_code(&envelope.message), Some("message.unsupported"));
        assert!(!reply.close);
        assert!(records.lock().unwrap().is_empty());
    }
}
