//! The core's journal of its runs: the ids it has issued and what each chat's latest run was
//! doing, so a restart can pick them up.

pub(crate) mod task;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, AgentSession, ChatId, RequestId, RunId, RunState, WorkspaceId, code,
};

use crate::platform;

#[derive(Debug)]
pub(crate) struct Runs {
    path: PathBuf,
    journal: Journal,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Journal {
    /// How many run ids were ever issued; it only grows, so no id comes back.
    issued: u64,
    chats: Vec<Record>,
}

/// A chat's latest run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    pub(crate) agent: AgentKind,
    /// `None` until the chat's first run starts.
    pub(crate) run: Option<RunId>,
    pub(crate) state: RunState,
    /// The agent's own conversation, once it has named it; the chat's next run resumes it.
    pub(crate) session: Option<AgentSession>,
    /// The command whose turn is running.
    pub(crate) turn: Option<RequestId>,
    /// Prompts accepted and not yet started, oldest first.
    pub(crate) queue: Vec<Queued>,
    /// Unix seconds when its last turn started or ended.
    pub(crate) active_at: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Queued {
    pub(crate) request: RequestId,
    pub(crate) text: String,
}

// The text is content, which never reaches a log line, even through `{:?}`.
impl fmt::Debug for Queued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queued")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl Runs {
    /// The journal in `state_dir`, empty when there is none yet.
    ///
    /// # Errors
    /// When the journal can't be read: starting over would hand out old run ids again.
    pub(crate) fn load(state_dir: &Path) -> Result<Self, Fault> {
        let path = state_dir.join("runs.json");
        let unreadable = || Fault::new(code!("runs.unreadable"), "The runs journal can't be read");
        let journal = match std::fs::read(&path) {
            Ok(json) => {
                serde_json::from_slice(&json).map_err(|error| unreadable().because(error))?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Journal::default(),
            Err(error) => return Err(unreadable().because(error)),
        };
        Ok(Self { path, journal })
    }

    pub(crate) fn of(&self, workspace: &WorkspaceId, chat: &ChatId) -> Option<&Record> {
        self.journal.chats.get(self.position(workspace, chat)?)
    }

    /// Starts the chat's next run under an id never issued before, keeping the chat's queue and,
    /// for the same agent, its session. The id is saved before it is returned. Not safe to
    /// cancel, like [`Self::update`]: two saves must never overlap.
    pub(crate) async fn begin(
        &mut self,
        workspace: &WorkspaceId,
        chat: &ChatId,
        agent: AgentKind,
        now: u64,
    ) -> Result<RunId, Fault> {
        // Kept even when the save fails, so the id is never tried again.
        self.journal.issued += 1;
        let run = RunId::try_from(format!("run-{}", self.journal.issued))
            .map_err(|_| Fault::new(code!("runs.no_id"), "No run id could be made"))?;
        let mut next = self.journal.clone();
        let (session, queue) = self
            .position(workspace, chat)
            .map(|at| next.chats.swap_remove(at))
            .map(|record| {
                (
                    record.session.filter(|_| record.agent == agent),
                    record.queue,
                )
            })
            .unwrap_or_default();
        next.chats.push(Record {
            workspace: workspace.clone(),
            chat: chat.clone(),
            agent,
            run: Some(run.clone()),
            state: RunState::Starting,
            session,
            turn: None,
            queue,
            active_at: now,
        });
        self.commit(next).await?;
        Ok(run)
    }

    /// Queues a prompt for the chat, which gets a record for `agent` when it has none. A
    /// prompt already queued or running under the same request isn't queued again.
    pub(crate) async fn enqueue(
        &mut self,
        (workspace, chat): (&WorkspaceId, &ChatId),
        agent: AgentKind,
        prompt: Queued,
        now: u64,
    ) -> Result<(), Fault> {
        let mut next = self.journal.clone();
        let at = self.position(workspace, chat).unwrap_or(next.chats.len());
        if at == next.chats.len() {
            next.chats.push(Record {
                workspace: workspace.clone(),
                chat: chat.clone(),
                agent,
                run: None,
                state: RunState::Sleeping,
                session: None,
                turn: None,
                queue: Vec::new(),
                active_at: now,
            });
        }
        let Some(record) = next.chats.get_mut(at) else {
            return Ok(());
        };
        let known = record.turn.as_ref() == Some(&prompt.request)
            || record
                .queue
                .iter()
                .any(|queued| queued.request == prompt.request);
        if known {
            return Ok(());
        }
        record.queue.push(prompt);
        self.commit(next).await
    }

    /// Changes the chat's record and saves it; a change that can't be saved isn't made. Does
    /// nothing when the chat has no record.
    pub(crate) async fn update(
        &mut self,
        workspace: &WorkspaceId,
        chat: &ChatId,
        change: impl FnOnce(&mut Record),
    ) -> Result<(), Fault> {
        let mut next = self.journal.clone();
        let at = self.position(workspace, chat);
        let Some(record) = at.and_then(|at| next.chats.get_mut(at)) else {
            return Ok(());
        };
        change(record);
        self.commit(next).await
    }

    /// Whether `run` is the chat's latest run: an earlier run's, or another chat's, never is.
    pub(crate) fn is_latest(&self, workspace: &WorkspaceId, chat: &ChatId, run: &RunId) -> bool {
        self.of(workspace, chat)
            .is_some_and(|record| record.run.as_ref() == Some(run))
    }

    fn position(&self, workspace: &WorkspaceId, chat: &ChatId) -> Option<usize> {
        self.journal
            .chats
            .iter()
            .position(|record| record.workspace == *workspace && record.chat == *chat)
    }

    async fn commit(&mut self, next: Journal) -> Result<(), Fault> {
        let not_saved = || Fault::new(code!("runs.not_saved"), "The runs journal can't be saved");
        let json = serde_json::to_vec(&next).map_err(|error| not_saved().because(error))?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || platform::write_whole(&path, &json))
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)))
            .map_err(|error| not_saved().because(error))?;
        self.journal = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn workspace(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn chat(id: &str) -> ChatId {
        ChatId::try_from(id.to_owned()).unwrap()
    }

    async fn begin(runs: &mut Runs, chat_id: &str) -> RunId {
        runs.begin(&workspace("w-1"), &chat(chat_id), AgentKind::Claude, 100)
            .await
            .unwrap()
    }

    async fn waiting(runs: &mut Runs, chat_id: &str) {
        runs.update(&workspace("w-1"), &chat(chat_id), |record| {
            record.state = RunState::Waiting;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_run_id_is_never_issued_twice_even_after_a_restart() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();

        let first = begin(&mut runs, "c-1").await;
        let second = begin(&mut runs, "c-1").await;
        let mut restarted = Runs::load(&dir).unwrap();
        let third = begin(&mut restarted, "c-2").await;

        assert_eq!(
            [first.as_str(), second.as_str(), third.as_str()],
            ["run-1", "run-2", "run-3"]
        );
    }

    #[tokio::test]
    async fn a_change_that_cant_be_saved_isnt_made_and_its_id_is_never_issued_again() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let (w1, c1) = (workspace("w-1"), chat("c-1"));
        let asked = begin(&mut runs, "c-1").await;
        waiting(&mut runs, "c-1").await;
        std::fs::remove_file(dir.join("runs.json")).unwrap();
        std::fs::create_dir(dir.join("runs.json")).unwrap();

        let begun = runs.begin(&w1, &c1, AgentKind::Claude, 100).await;
        let updated = runs
            .update(&w1, &c1, |record| record.state = RunState::Idle)
            .await;
        std::fs::remove_dir(dir.join("runs.json")).unwrap();

        assert_eq!(begun.unwrap_err().code().as_str(), "runs.not_saved");
        assert_eq!(updated.unwrap_err().code().as_str(), "runs.not_saved");
        assert!(runs.is_latest(&w1, &c1, &asked));
        assert_eq!(begin(&mut runs, "c-1").await.as_str(), "run-3");
    }

    #[tokio::test]
    async fn another_agent_doesnt_resume_the_first_ones_session() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let (w1, c1) = (workspace("w-1"), chat("c-1"));
        begin(&mut runs, "c-1").await;
        runs.update(&w1, &c1, |record| {
            record.session = AgentSession::try_from(SESSION.to_owned()).ok();
        })
        .await
        .unwrap();

        runs.begin(&w1, &c1, AgentKind::Codex, 100).await.unwrap();

        assert_eq!(runs.of(&w1, &c1).unwrap().session, None);
    }

    #[tokio::test]
    async fn a_permission_answer_only_reaches_the_run_that_asked() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let asked = begin(&mut runs, "c-1").await;
        let other = begin(&mut runs, "c-2").await;
        let (w1, c1) = (workspace("w-1"), chat("c-1"));

        waiting(&mut runs, "c-1").await;
        waiting(&mut runs, "c-2").await;

        assert!(runs.is_latest(&w1, &c1, &asked));
        assert!(!runs.is_latest(&w1, &c1, &other));
        assert!(!runs.is_latest(&w1, &chat("c-3"), &asked));
        assert!(!runs.is_latest(&workspace("w-2"), &c1, &asked));

        let mut restarted = Runs::load(&dir).unwrap();
        assert!(restarted.is_latest(&w1, &c1, &asked));
        let next = begin(&mut restarted, "c-1").await;
        waiting(&mut restarted, "c-1").await;

        assert!(!restarted.is_latest(&w1, &c1, &asked));
        assert!(restarted.is_latest(&w1, &c1, &next));
    }

    #[tokio::test]
    async fn a_chats_next_run_keeps_its_agent_session_and_queue() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let (w1, c1) = (workspace("w-1"), chat("c-1"));
        let queued = Queued {
            request: RequestId::try_from("r-2".to_owned()).unwrap(),
            text: "Then the docs".to_owned(),
        };
        begin(&mut runs, "c-1").await;
        runs.update(&w1, &c1, |record| {
            record.session = AgentSession::try_from(SESSION.to_owned()).ok();
            record.turn = RequestId::try_from("r-1".to_owned()).ok();
            record.queue.push(queued.clone());
        })
        .await
        .unwrap();
        runs.update(&w1, &chat("c-9"), |record| record.active_at = 0)
            .await
            .unwrap();

        let mut restarted = Runs::load(&dir).unwrap();
        let run = restarted
            .begin(&w1, &c1, AgentKind::Claude, 200)
            .await
            .unwrap();

        assert_eq!(
            Runs::load(&dir).unwrap().of(&w1, &c1),
            Some(&Record {
                workspace: w1.clone(),
                chat: c1.clone(),
                agent: AgentKind::Claude,
                run: Some(run),
                state: RunState::Starting,
                session: AgentSession::try_from(SESSION.to_owned()).ok(),
                turn: None,
                queue: vec![queued],
                active_at: 200,
            })
        );
        assert_eq!(restarted.of(&w1, &chat("c-9")), None);
    }

    #[test]
    fn a_journal_that_cant_be_read_stops_the_start() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.join("runs.json"), b"{").unwrap();
        let torn = Runs::load(&dir).unwrap_err();
        std::fs::remove_file(dir.join("runs.json")).unwrap();
        std::fs::create_dir(dir.join("runs.json")).unwrap();
        let folder = Runs::load(&dir).unwrap_err();

        assert_eq!(torn.code().as_str(), "runs.unreadable");
        assert_eq!(folder.code().as_str(), "runs.unreadable");
    }

    #[test]
    fn queued_text_stays_out_of_debug_output() {
        let queued = Queued {
            request: RequestId::try_from("r-1".to_owned()).unwrap(),
            text: "MARKER".to_owned(),
        };

        assert!(!format!("{queued:?}").contains("MARKER"));
        assert!(format!("{queued:?}").contains("r-1"));
    }

    #[tokio::test]
    async fn a_prompt_is_queued_once_and_a_new_chat_gets_a_record_without_a_run() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let (w1, c1, c2) = (workspace("w-1"), chat("c-1"), chat("c-2"));
        let queued = |id: &str| Queued {
            request: RequestId::try_from(id.to_owned()).unwrap(),
            text: "Hi".to_owned(),
        };
        let agent = AgentKind::Codex;

        runs.enqueue((&w1, &c1), agent, queued("r-1"), 7)
            .await
            .unwrap();
        runs.enqueue((&w1, &c1), agent, queued("r-1"), 8)
            .await
            .unwrap();
        let record = Runs::load(&dir).unwrap().of(&w1, &c1).cloned().unwrap();
        let fresh = Record {
            workspace: w1.clone(),
            chat: c1.clone(),
            agent,
            run: None,
            state: RunState::Sleeping,
            session: None,
            turn: None,
            queue: vec![queued("r-1")],
            active_at: 7,
        };
        assert_eq!(record, fresh);
        assert!(!runs.is_latest(&w1, &c1, &RunId::try_from("run-1".to_owned()).unwrap()));

        // A prompt on its turn isn't queued again, and another chat's record is left alone.
        runs.update(&w1, &c1, |record| {
            record.turn = record.queue.pop().map(|queued| queued.request);
        })
        .await
        .unwrap();
        runs.enqueue((&w1, &c1), agent, queued("r-1"), 9)
            .await
            .unwrap();
        runs.enqueue((&w1, &c1), agent, queued("r-2"), 9)
            .await
            .unwrap();
        runs.enqueue((&w1, &c2), agent, queued("r-1"), 9)
            .await
            .unwrap();

        assert_eq!(runs.of(&w1, &c1).unwrap().queue, [queued("r-2")]);
        assert_eq!(runs.of(&w1, &c2).unwrap().queue, [queued("r-1")]);
        assert_eq!(begin(&mut runs, "c-1").await.as_str(), "run-1");
        assert_eq!(runs.of(&w1, &c1).unwrap().queue, [queued("r-2")]);
    }
}
