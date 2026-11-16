//! Answers a person's questions about the sessions they ran outside Weacon, read from their
//! own agent folders and never stored.

use std::path::{Path, PathBuf};

use weacon_agents::outside;
use weacon_agents::sessions::Disk;
use weacon_observability::{Fault, Logger};
use weacon_protocol::{AgentKind, SessionsAnswer, SessionsQuery, code};

#[derive(Clone, Default)]
pub(crate) struct Outside {
    homes: Vec<(AgentKind, PathBuf)>,
    state_dir: Option<PathBuf>,
}

impl Outside {
    /// The person's own agent folders; any in or around `state_dir`, where each workspace's
    /// sign-ins are kept, is never read.
    pub(crate) fn new(homes: Vec<(AgentKind, PathBuf)>, state_dir: Option<&Path>) -> Self {
        Self {
            homes,
            state_dir: state_dir.map(Path::to_owned),
        }
    }

    // The folders to read now, compared as resolved, since one may appear or change after start.
    fn readable(&self) -> Vec<(AgentKind, PathBuf)> {
        let state_dir = self.state_dir.as_deref().map(resolved);
        let apart = |dir: &Path| {
            let dir = resolved(dir);
            state_dir
                .as_ref()
                .is_none_or(|state| !dir.starts_with(state) && !state.starts_with(&dir))
        };
        self.homes
            .iter()
            .filter(|(_, dir)| apart(dir))
            .cloned()
            .collect()
    }

    /// The answer to `query`, for the machine's owner, whoever checked that.
    ///
    /// # Errors
    /// When the sessions can't be read.
    pub(crate) async fn answer(
        &self,
        query: SessionsQuery,
        log: &Logger,
    ) -> Result<SessionsAnswer, Fault> {
        let outside = self.clone();
        let kind = query.query;
        let answer =
            tokio::task::spawn_blocking(move || outside::answer(&Disk, &outside.readable(), &kind))
                .await
                .map_err(|error| {
                    Fault::new(
                        code!("sessions.read_failed"),
                        "The sessions couldn't be read",
                    )
                    .because(error)
                })?;
        let request = query.request.as_str().into();
        log.info("sessions answered", &[("request_id", request)]);
        Ok(SessionsAnswer {
            request: query.request,
            machine: query.machine,
            answer,
        })
    }
}

fn resolved(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{
        AgentSession, ChatEvent, MachineId, RequestId, SessionsAnswerKind, SessionsQueryKind,
    };
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::test_support::{SESSION, claude_home};

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

    async fn listed(outside: &Outside) -> usize {
        let (log, _) = recording_logger();
        let answer = outside.answer(query(SessionsQueryKind::List), &log).await;
        let SessionsAnswerKind::List { sessions } = answer.unwrap().answer else {
            panic!("expected a list");
        };
        sessions.len()
    }

    #[tokio::test]
    async fn folders_in_or_around_the_state_folder_are_never_read() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        let inside = claude_home(&state.join("w-1"), "/work", "Hi");
        let own = claude_home(&dir.join("own"), "/work", "Hi");
        let homes = |dirs: &[&PathBuf]| -> Vec<_> {
            dirs.iter()
                .map(|dir| (AgentKind::Claude, PathBuf::clone(dir)))
                .collect()
        };

        let kept = Outside::new(homes(&[&own]), Some(&state));
        let around = Outside::new(homes(&[&dir.to_path_buf(), &inside]), Some(&state));
        let unplaced = Outside::new(homes(&[&inside]), None);

        assert_eq!(listed(&kept).await, 1);
        assert_eq!(listed(&around).await, 0);
        assert_eq!(listed(&unplaced).await, 1);
    }

    #[tokio::test]
    async fn a_folder_made_in_the_state_folder_after_start_is_never_read() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(dir.join("other")).unwrap();
        let roundabout = dir
            .join("other")
            .join("..")
            .join("state")
            .join("w-1")
            .join("claude");
        let outside = Outside::new(vec![(AgentKind::Claude, roundabout)], Some(&state));

        claude_home(&state.join("w-1"), "/work", "Hi");

        assert_eq!(listed(&outside).await, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_linked_into_the_state_folder_is_never_read() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        let inside = claude_home(&state.join("w-1"), "/work", "Hi");
        let linked = dir.join("linked");
        std::os::unix::fs::symlink(&inside, &linked).unwrap();

        let outside = Outside::new(vec![(AgentKind::Claude, linked)], Some(&state));

        assert_eq!(listed(&outside).await, 0);
    }

    #[tokio::test]
    async fn the_owner_sees_the_list_and_a_session_and_no_content_reaches_the_log() {
        let dir = TempDir::new().unwrap();
        let home = claude_home(&dir, "/MARKER-FOLDER", "MARKER-PROMPT");
        let outside = Outside::new(vec![(AgentKind::Claude, home)], Some(&dir.join("state")));
        let (log, records) = recording_logger();

        let listed = outside
            .answer(query(SessionsQueryKind::List), &log)
            .await
            .unwrap();
        let opened = outside.answer(query(open()), &log).await.unwrap();

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
}
