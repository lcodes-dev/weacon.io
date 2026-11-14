//! The core's journal of its runs: the ids it has issued and what each chat's latest run was
//! doing, so a restart can pick them up.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, AgentSession, ChatId, RequestId, RunId, RunState, WorkspaceId, code,
};

use crate::app::{blocking, read_json};

#[derive(Debug)]
pub(crate) struct Runs {
    path: PathBuf,
    journal: Journal,
}

/// The journal as it is on disk, where each chat's record is read on its own.
#[derive(Default, Serialize, Deserialize)]
struct Stored {
    issued: u64,
    chats: Vec<Box<RawValue>>,
}

#[derive(Clone, Debug, Default)]
struct Journal {
    /// How many run ids were ever issued; it only grows, so no id comes back.
    issued: u64,
    chats: Vec<Record>,
    /// Records this core can't read in full, as a newer one writes; kept as found.
    newer: Vec<Kept>,
}

/// A record kept as the JSON it was read as, and the chat it is for when that much is known.
#[derive(Clone)]
struct Kept {
    key: Option<ChatKey>,
    json: Box<RawValue>,
}

// The JSON holds prompts, which never reach a log line, even through `{:?}`.
impl fmt::Debug for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kept")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// Which chat: its workspace and its id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ChatKey {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
}

impl ChatKey {
    pub(crate) fn new(workspace: &WorkspaceId, chat: &ChatId) -> Self {
        Self {
            workspace: workspace.clone(),
            chat: chat.clone(),
        }
    }
}

/// A chat's latest run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    #[serde(flatten)]
    pub(crate) key: ChatKey,
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
        let stored: Stored = read_json(&path)
            .map_err(|error| unreadable().because(error))?
            .unwrap_or_default();
        let mut journal = Journal {
            issued: stored.issued,
            ..Journal::default()
        };
        for json in stored.chats {
            match serde_json::from_str::<Record>(json.get()) {
                Ok(record) if !record.is_newer() => journal.chats.push(record),
                _ => journal.newer.push(Kept {
                    key: serde_json::from_str(json.get()).ok(),
                    json,
                }),
            }
        }
        Ok(Self { path, journal })
    }

    /// Refuses a chat whose record only a newer core can read, which is left to it.
    ///
    /// # Errors
    /// `runs.newer_record` for such a chat.
    pub(crate) fn refuse_newer(&self, key: &ChatKey) -> Result<(), Fault> {
        if self
            .journal
            .newer
            .iter()
            .any(|kept| kept.key.as_ref() == Some(key))
        {
            return Err(Fault::new(
                code!("runs.newer_record"),
                "A newer core last ran this chat",
            ));
        }
        Ok(())
    }

    pub(crate) fn of(&self, key: &ChatKey) -> Option<&Record> {
        self.journal.chats.get(self.position(key)?)
    }

    /// Starts the chat's next run under an id never issued before, keeping the chat's queue and,
    /// for the same agent, its session. The id is saved before it is returned. Not safe to
    /// cancel, like [`Self::update`]: two saves must never overlap.
    pub(crate) async fn begin(
        &mut self,
        key: &ChatKey,
        agent: AgentKind,
        now: u64,
    ) -> Result<RunId, Fault> {
        let no_id = || Fault::new(code!("runs.no_id"), "No run id could be made");
        self.refuse_newer(key)?;
        // Kept even when the save fails, so the id is never tried again.
        self.journal.issued = self.journal.issued.checked_add(1).ok_or_else(no_id)?;
        let run = RunId::try_from(format!("run-{}", self.journal.issued)).map_err(|_| no_id())?;
        let mut next = self.journal.clone();
        let (session, queue) = self
            .position(key)
            .map(|at| next.chats.swap_remove(at))
            .map(|record| {
                (
                    record.session.filter(|_| record.agent == agent),
                    record.queue,
                )
            })
            .unwrap_or_default();
        next.chats.push(Record {
            key: key.clone(),
            agent,
            run: Some(run.clone()),
            state: RunState::Starting,
            session,
            turn: None,
            queue,
            active_at: now,
        });
        self.save(next).await?;
        Ok(run)
    }

    /// Queues a prompt for the chat, which gets a record for `agent` when it has none. A
    /// prompt already queued or running under the same request isn't queued again.
    pub(crate) async fn enqueue(
        &mut self,
        key: &ChatKey,
        agent: AgentKind,
        prompt: Queued,
        now: u64,
    ) -> Result<(), Fault> {
        self.refuse_newer(key)?;
        let mut next = self.journal.clone();
        let at = self.position(key).unwrap_or(next.chats.len());
        if at == next.chats.len() {
            next.chats.push(Record {
                key: key.clone(),
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
        self.save(next).await
    }

    /// Changes the chat's record and saves it; a change that can't be saved isn't made. Does
    /// nothing when the chat has no record.
    pub(crate) async fn update(
        &mut self,
        key: &ChatKey,
        change: impl FnOnce(&mut Record),
    ) -> Result<(), Fault> {
        self.refuse_newer(key)?;
        let mut next = self.journal.clone();
        let at = self.position(key);
        let Some(record) = at.and_then(|at| next.chats.get_mut(at)) else {
            return Ok(());
        };
        change(record);
        self.save(next).await
    }

    fn position(&self, key: &ChatKey) -> Option<usize> {
        self.journal
            .chats
            .iter()
            .position(|record| record.key == *key)
    }

    /// Makes every save fail, as on a full disk, until [`Runs::unjam`] gets the path back.
    #[cfg(test)]
    pub(crate) fn jam(&mut self) -> PathBuf {
        let dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        std::mem::replace(&mut self.path, dir)
    }

    #[cfg(test)]
    pub(crate) fn unjam(&mut self, path: PathBuf) {
        self.path = path;
    }

    async fn save(&mut self, next: Journal) -> Result<(), Fault> {
        let not_saved = || Fault::new(code!("runs.not_saved"), "The runs journal can't be saved");
        let json = stored(&next).map_err(|error| not_saved().because(error))?;
        let path = self.path.clone();
        blocking(move || weacon_appkit::write_private(&path, &json))
            .await
            .map_err(|error| not_saved().because(error))?;
        self.journal = next;
        Ok(())
    }
}

impl Record {
    const fn is_newer(&self) -> bool {
        matches!(self.agent, AgentKind::Newer) || matches!(self.state, RunState::Newer)
    }
}

// The journal's bytes: the records this core can read, then the kept ones as they were found.
fn stored(journal: &Journal) -> serde_json::Result<Vec<u8>> {
    let read = journal.chats.iter().map(serde_json::value::to_raw_value);
    let mut chats = read.collect::<serde_json::Result<Vec<_>>>()?;
    chats.extend(journal.newer.iter().map(|kept| kept.json.clone()));
    let issued = journal.issued;
    serde_json::to_vec(&Stored { issued, chats })
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::chats::test_support::{run, session};

    fn key(workspace: &str, chat: &str) -> ChatKey {
        ChatKey {
            workspace: WorkspaceId::try_from(workspace.to_owned()).unwrap(),
            chat: ChatId::try_from(chat.to_owned()).unwrap(),
        }
    }

    async fn begin(runs: &mut Runs, chat_id: &str) -> RunId {
        runs.begin(&key("w-1", chat_id), AgentKind::Claude, 100)
            .await
            .unwrap()
    }

    async fn waiting(runs: &mut Runs, chat_id: &str) {
        runs.update(&key("w-1", chat_id), |record| {
            record.state = RunState::Waiting;
        })
        .await
        .unwrap();
    }

    fn latest(runs: &Runs, key: &ChatKey) -> Option<RunId> {
        runs.of(key)?.run.clone()
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
        let c1 = key("w-1", "c-1");
        let first = begin(&mut runs, "c-1").await;
        waiting(&mut runs, "c-1").await;
        std::fs::remove_file(dir.join("runs.json")).unwrap();
        std::fs::create_dir(dir.join("runs.json")).unwrap();

        let begun = runs.begin(&c1, AgentKind::Claude, 100).await;
        let updated = runs
            .update(&c1, |record| record.state = RunState::Idle)
            .await;
        std::fs::remove_dir(dir.join("runs.json")).unwrap();

        assert_eq!(begun.unwrap_err().code().as_str(), "runs.not_saved");
        assert_eq!(updated.unwrap_err().code().as_str(), "runs.not_saved");
        let record = runs.of(&c1).unwrap();
        assert_eq!(
            (&record.run, record.state),
            (&Some(first), RunState::Waiting)
        );
        assert_eq!(begin(&mut runs, "c-1").await.as_str(), "run-3");
    }

    #[tokio::test]
    async fn another_agent_doesnt_resume_the_first_ones_session() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let c1 = key("w-1", "c-1");
        begin(&mut runs, "c-1").await;
        runs.update(&c1, |record| {
            record.session = Some(session());
        })
        .await
        .unwrap();

        runs.begin(&c1, AgentKind::Codex, 100).await.unwrap();

        assert_eq!(runs.of(&c1).unwrap().session, None);
    }

    #[tokio::test]
    async fn each_chat_has_its_own_latest_run_and_keeps_it_across_a_restart() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let first = begin(&mut runs, "c-1").await;
        let other = begin(&mut runs, "c-2").await;
        let c1 = key("w-1", "c-1");

        assert_eq!(latest(&runs, &c1), Some(first.clone()));
        assert_eq!(latest(&runs, &key("w-1", "c-2")), Some(other));
        assert_eq!(latest(&runs, &key("w-1", "c-3")), None);
        assert_eq!(latest(&runs, &key("w-2", "c-1")), None);

        let mut restarted = Runs::load(&dir).unwrap();
        assert_eq!(latest(&restarted, &c1), Some(first));
        let next = begin(&mut restarted, "c-1").await;
        assert_eq!(latest(&restarted, &c1), Some(next));
    }

    #[tokio::test]
    async fn a_chats_next_run_keeps_its_agent_session_and_queue() {
        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let c1 = key("w-1", "c-1");
        let queued = Queued {
            request: RequestId::try_from("r-2".to_owned()).unwrap(),
            text: "Then the docs".to_owned(),
        };
        begin(&mut runs, "c-1").await;
        runs.update(&c1, |record| {
            record.session = Some(session());
            record.turn = RequestId::try_from("r-1".to_owned()).ok();
            record.queue.push(queued.clone());
        })
        .await
        .unwrap();
        runs.update(&key("w-1", "c-9"), |record| record.active_at = 0)
            .await
            .unwrap();

        let mut restarted = Runs::load(&dir).unwrap();
        let run = restarted.begin(&c1, AgentKind::Claude, 200).await.unwrap();

        assert_eq!(
            Runs::load(&dir).unwrap().of(&c1),
            Some(&Record {
                key: c1.clone(),
                agent: AgentKind::Claude,
                run: Some(run),
                state: RunState::Starting,
                session: Some(session()),
                turn: None,
                queue: vec![queued],
                active_at: 200,
            })
        );
        assert_eq!(restarted.of(&key("w-1", "c-9")), None);
    }

    #[tokio::test]
    async fn a_counter_at_its_end_issues_no_more_ids() {
        let dir = TempDir::new().unwrap();
        let last = format!(r#"{{"issued":{},"chats":[]}}"#, u64::MAX - 1);
        std::fs::write(dir.join("runs.json"), last).unwrap();
        let mut runs = Runs::load(&dir).unwrap();
        let c1 = key("w-1", "c-1");

        let last = begin(&mut runs, "c-1").await;
        let spent = runs.begin(&c1, AgentKind::Claude, 100).await;

        assert_eq!(last.as_str(), format!("run-{}", u64::MAX));
        assert_eq!(spent.unwrap_err().code().as_str(), "runs.no_id");
        assert_eq!(runs.of(&c1).unwrap().run, Some(last));
    }

    #[tokio::test]
    async fn a_record_keeps_its_workspace_and_chat_at_its_top_level() {
        let dir = TempDir::new().unwrap();
        let saved = r#"{"issued":3,"chats":[{"workspace":"w-1","chat":"c-1","agent":"claude",
            "run":"run-3","state":"idle","session":null,"turn":null,"queue":[],"active_at":5}]}"#;
        std::fs::write(dir.join("runs.json"), saved).unwrap();

        let mut runs = Runs::load(&dir).unwrap();
        assert_eq!(
            latest(&runs, &key("w-1", "c-1")),
            RunId::try_from("run-3".to_owned()).ok()
        );
        begin(&mut runs, "c-1").await;

        let raw = std::fs::read_to_string(dir.join("runs.json")).unwrap();
        let journal: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let record = &journal["chats"][0];
        assert_eq!(
            (&record["workspace"], &record["chat"]),
            (&"w-1".into(), &"c-1".into())
        );
        assert_eq!(record["run"], "run-4");
        assert!(record.get("key").is_none());
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
        let (c1, c2) = (key("w-1", "c-1"), key("w-1", "c-2"));
        let queued = |id: &str| Queued {
            request: RequestId::try_from(id.to_owned()).unwrap(),
            text: "Hi".to_owned(),
        };
        let agent = AgentKind::Codex;

        runs.enqueue(&c1, agent, queued("r-1"), 7).await.unwrap();
        runs.enqueue(&c1, agent, queued("r-1"), 8).await.unwrap();
        let record = Runs::load(&dir).unwrap().of(&c1).cloned().unwrap();
        let fresh = Record {
            key: c1.clone(),
            agent,
            run: None,
            state: RunState::Sleeping,
            session: None,
            turn: None,
            queue: vec![queued("r-1")],
            active_at: 7,
        };
        assert_eq!(record, fresh);
        assert_ne!(
            latest(&runs, &c1).as_ref(),
            Some(&RunId::try_from("run-1".to_owned()).unwrap())
        );

        // A prompt on its turn isn't queued again, and another chat's record is left alone.
        runs.update(&c1, |record| {
            record.turn = record.queue.pop().map(|queued| queued.request);
        })
        .await
        .unwrap();
        runs.enqueue(&c1, agent, queued("r-1"), 9).await.unwrap();
        runs.enqueue(&c1, agent, queued("r-2"), 9).await.unwrap();
        runs.enqueue(&c2, agent, queued("r-1"), 9).await.unwrap();

        assert_eq!(runs.of(&c1).unwrap().queue, [queued("r-2")]);
        assert_eq!(runs.of(&c2).unwrap().queue, [queued("r-1")]);
        assert_eq!(begin(&mut runs, "c-1").await.as_str(), "run-1");
        assert_eq!(runs.of(&c1).unwrap().queue, [queued("r-2")]);
    }

    // What beginning, queueing for and changing `key`'s record each fail with.
    async fn refusals(runs: &mut Runs, key: &ChatKey) -> Vec<String> {
        let queued = Queued {
            request: RequestId::try_from("r-2".to_owned()).unwrap(),
            text: "More".to_owned(),
        };
        let tries = [
            runs.begin(key, AgentKind::Claude, 9).await.map(drop),
            runs.enqueue(key, AgentKind::Claude, queued, 9).await,
            runs.update(key, |record| record.active_at = 9).await,
        ];
        let code = |tried: Result<(), Fault>| tried.unwrap_err().code().as_str().to_owned();
        tries.into_iter().map(code).collect()
    }

    // A record a newer core wrote: an agent and a field this core doesn't know.
    const NEWER: &str = r#"{"workspace":"w-1","chat":"c-9","agent":"gemini","run":"run-2","state":"idle","session":null,"turn":null,"queue":[{"request":"r-1","text":"Hi"}],"active_at":5,"colour":"blue"}"#;

    #[tokio::test]
    async fn a_record_only_a_newer_core_reads_is_kept_as_found_and_its_chat_left_to_it() {
        let dir = TempDir::new().unwrap();
        let broken = r#"{"workspace":"w-1","chat":"c-8","state":"paused"}"#;
        let nameless = r#"{"shape":"unknown"}"#;
        let journal = format!(r#"{{"issued":3,"chats":[{NEWER},{broken},{nameless}]}}"#);
        std::fs::write(dir.join("runs.json"), journal).unwrap();

        let mut runs = Runs::load(&dir).unwrap();
        for chat in ["c-9", "c-8"] {
            let kept = key("w-1", chat);
            assert_eq!(runs.of(&kept), None);
            assert_eq!(refusals(&mut runs, &kept).await, ["runs.newer_record"; 3]);
        }

        assert_eq!(begin(&mut runs, "c-1").await.as_str(), "run-4");
        let raw = std::fs::read_to_string(dir.join("runs.json")).unwrap();
        for kept in [NEWER, broken, nameless] {
            assert!(raw.contains(kept), "{kept} was rewritten");
        }
        let restarted = Runs::load(&dir).unwrap();
        assert_eq!(latest(&restarted, &key("w-1", "c-1")), Some(run("run-4")));
        assert_eq!(restarted.of(&key("w-1", "c-9")), None);
    }

    #[test]
    fn a_kept_record_stays_out_of_debug_output() {
        let dir = TempDir::new().unwrap();
        let journal = format!(r#"{{"issued":3,"chats":[{NEWER}]}}"#);
        std::fs::write(dir.join("runs.json"), journal).unwrap();

        let runs = Runs::load(&dir).unwrap();

        assert!(!format!("{runs:?}").contains("Hi"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_journal_is_readable_only_by_its_user() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let mut runs = Runs::load(&dir).unwrap();

        begin(&mut runs, "c-1").await;

        let mode = std::fs::metadata(dir.join("runs.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
