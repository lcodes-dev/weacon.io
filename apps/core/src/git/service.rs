//! The core's answers to its clients about chats' worktrees: their changes, where they stand,
//! and what a person asks of them. The daemon's clients are its owner's alone.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use tokio::sync::Semaphore;

use weacon_observability::Fault;
use weacon_protocol::{
    ChatId, DiffAnswer, DiffAnswerKind, DiffContent, DiffQuery, DiffQueryKind, Envelope,
    FrameError, Message, WorkspaceId, WorktreeAction, WorktreeAnswer, WorktreeAnswerKind,
    WorktreeCommand, WorktreeQuery, WorktreeQueryKind, code, encode_line,
};

use super::branch::{Draft, Gh};
use super::chats::{ChatRepo, ChatRepos};
use super::removal::Journal;
use super::repos::Repos;
use super::{Git, branch, diff};
use crate::platform::trash::Trashed;
use crate::workspaces::Workspaces;

/// Moves an item to the person's Trash; tests pass one of their own.
pub(crate) type ToTrash = fn(&Path, SystemTime) -> io::Result<Trashed>;

// At most this many calls run git at once; the rest wait their turn without a thread.
const WORKERS: usize = 4;

/// What answers about worktrees, a few calls at a time.
#[derive(Clone)]
pub(crate) struct Worktrees {
    inner: Arc<Inner>,
}

// A chat that has a worktree: its git, its record and where the worktree is.
struct Chat {
    git: Git,
    record: ChatRepo,
    worktree: PathBuf,
}

struct Inner {
    state_dir: PathBuf,
    path: Option<OsString>,
    work_root: Option<PathBuf>,
    to_trash: ToTrash,
    workers: Arc<Semaphore>,
    // Held while a journal changes; it guards no data, so a panic under it leaves none broken.
    journals: Mutex<()>,
}

impl Worktrees {
    /// Answers for the chats whose state is in `state_dir` and whose worktrees are in
    /// `work_root`, finding git and gh on `path`.
    pub(crate) fn new(
        state_dir: &Path,
        path: Option<OsString>,
        work_root: Option<PathBuf>,
        to_trash: ToTrash,
    ) -> Self {
        let inner = Inner {
            state_dir: state_dir.to_owned(),
            path,
            work_root,
            to_trash,
            workers: Arc::new(Semaphore::new(WORKERS)),
            journals: Mutex::new(()),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Every answer to `query`, in the order they're sent.
    pub(crate) async fn diff(&self, query: DiffQuery) -> Result<Vec<DiffAnswer>, Fault> {
        self.blocking(move |inner| inner.diff(&query)).await
    }

    pub(crate) async fn query(&self, query: WorktreeQuery) -> Result<WorktreeAnswer, Fault> {
        self.blocking(move |inner| {
            let answer = match query.query {
                WorktreeQueryKind::State => inner.state(&query.workspace, &query.chat)?,
                WorktreeQueryKind::Newer => return Err(unknown()),
            };
            Ok(WorktreeAnswer {
                request: query.request,
                workspace: query.workspace,
                chat: query.chat,
                answer,
            })
        })
        .await
    }

    pub(crate) async fn command(&self, command: WorktreeCommand) -> Result<WorktreeAnswer, Fault> {
        self.blocking(move |inner| {
            let WorktreeCommand {
                request,
                workspace,
                chat,
                action,
                ..
            } = command;
            let answer = inner.act(&workspace, &chat, action)?;
            Ok(WorktreeAnswer {
                request,
                workspace,
                chat,
                answer,
            })
        })
        .await
    }

    /// Waits for every call under way to end, and refuses any after.
    pub(crate) async fn drain(&self) {
        let workers = &self.inner.workers;
        let all = u32::try_from(WORKERS).unwrap_or(u32::MAX);
        // Kept until closed, so no call queued behind them starts in between.
        let held = workers.acquire_many(all).await;
        workers.close();
        drop(held);
    }

    // Runs `work` off the runtime once a worker is free. It runs to its end even when its
    // asker stops waiting, and keeps its worker until then.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Inner) -> Result<T, Fault> + Send + 'static,
    ) -> Result<T, Fault> {
        let inner = Arc::clone(&self.inner);
        let worker = Arc::clone(&inner.workers)
            .acquire_owned()
            .await
            .map_err(|_| failed())?;
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            work(&inner)
        })
        .await
        .map_err(|_| failed())?
    }
}

impl Inner {
    fn diff(&self, query: &DiffQuery) -> Result<Vec<DiffAnswer>, Fault> {
        if query.query != DiffQueryKind::Changes {
            return Err(unknown());
        }
        let Chat {
            git,
            record,
            worktree,
        } = self.chat(&query.workspace, &query.chat)?;
        let base = git.base(&worktree, &record.start)?;
        let files = git.changes(&worktree, &base)?;
        let answer = |answer| DiffAnswer {
            request: query.request.clone(),
            workspace: query.workspace.clone(),
            chat: query.chat.clone(),
            answer,
        };
        if files.is_empty() {
            return Ok(vec![answer(DiffAnswerKind::Unchanged)]);
        }
        let count = u32::try_from(files.len()).map_err(|_| diff::too_many())?;
        Ok(files
            .into_iter()
            .map(|file| answer(DiffAnswerKind::File { files: count, file }))
            .map(fitted)
            .collect())
    }

    fn state(&self, workspace: &WorkspaceId, chat: &ChatId) -> Result<WorktreeAnswerKind, Fault> {
        let Chat {
            git,
            record,
            worktree,
        } = self.chat(workspace, chat)?;
        if self.journal(workspace, &git).removed(chat)?.is_some() {
            // What it holds stays as it was, in the Trash.
            return Ok(WorktreeAnswerKind::State {
                removed: true,
                uncommitted: false,
                unpushed: false,
            });
        }
        let base = git.base(&worktree, &record.start)?;
        let unsaved = git.unsaved(&worktree, &base)?;
        Ok(WorktreeAnswerKind::State {
            removed: false,
            uncommitted: unsaved.uncommitted,
            unpushed: unsaved.unpushed,
        })
    }

    fn act(
        &self,
        workspace: &WorkspaceId,
        chat: &ChatId,
        action: WorktreeAction,
    ) -> Result<WorktreeAnswerKind, Fault> {
        let found = self.chat(workspace, chat)?;
        let journal = self.journal(workspace, &found.git);
        match action {
            WorktreeAction::Remove => {
                let _changing = self.journals.lock().unwrap_or_else(PoisonError::into_inner);
                if journal.removed(chat)?.is_none() {
                    let now = SystemTime::now();
                    let to_trash = |path: &Path| (self.to_trash)(path, now);
                    journal.remove(chat, &found.worktree, to_trash, now)?;
                }
                self.state(workspace, chat)
            }
            WorktreeAction::Restore => {
                let _changing = self.journals.lock().unwrap_or_else(PoisonError::into_inner);
                if journal.removed(chat)?.is_some() {
                    journal.restore(chat)?;
                }
                self.state(workspace, chat)
            }
            WorktreeAction::OpenPullRequest { title, body } => {
                self.pull_request(&found, &title, &body)
            }
            // Not offered by this core yet.
            WorktreeAction::OpenTerminal | WorktreeAction::Newer => Err(unknown()),
        }
    }

    // Pushes the chat's branch, then opens its draft pull request with the workspace's own gh,
    // or gives the page to open one on.
    fn pull_request(
        &self,
        chat: &Chat,
        title: &str,
        body: &str,
    ) -> Result<WorktreeAnswerKind, Fault> {
        let Chat {
            git,
            record,
            worktree,
        } = chat;
        let base = record.start.branch.as_deref().ok_or_else(|| {
            Fault::new(code!("pr.no_base"), "The chat's branch began on no branch")
        })?;
        let branch = branch(&record.chat);
        git.push(worktree, &branch)?;
        let config = self.root(&record.workspace).join("gh");
        let gh = Gh::find(self.path.as_deref(), config).map(|gh| gh.in_tree(git));
        if let Some(gh) = gh.filter(|gh| gh.signed_in(worktree)) {
            let draft = Draft {
                base,
                branch: &branch,
                title,
                body,
            };
            let url = gh.open_draft(worktree, &draft)?;
            return Ok(WorktreeAnswerKind::PullRequestOpened { url });
        }
        let url = git.compare_page(worktree, base, &branch)?;
        Ok(WorktreeAnswerKind::PullRequestPage { url })
    }

    // The chat's git, record and worktree; a chat that hasn't one is refused.
    fn chat(&self, workspace: &WorkspaceId, chat: &ChatId) -> Result<Chat, Fault> {
        let records = ChatRepos::new(
            workspace.clone(),
            self.root(workspace).join("git/chats.json"),
        );
        let none = || Fault::new(code!("worktree.none"), "The chat has no worktree");
        let record = records.get(chat)?.ok_or_else(none)?;
        let work_root = self.work_root.as_deref().ok_or_else(none)?;
        let worktree = crate::chats::chat_folder(work_root, workspace, chat);
        let repos = Repos::new(
            workspace.clone(),
            self.root(workspace).join("git/repos.json"),
        );
        let repo = repos.get(&record.repo)?.ok_or_else(|| {
            Fault::new(
                code!("repo.unknown"),
                "The chat's repository isn't registered",
            )
        })?;
        let git =
            Git::open(&self.state_dir, self.path.as_deref())?.in_tree(&repo.path, &worktree)?;
        Ok(Chat {
            git,
            record,
            worktree,
        })
    }

    fn journal(&self, workspace: &WorkspaceId, git: &Git) -> Journal {
        let file = self.root(workspace).join("git/removed.json");
        Journal::new(workspace.clone(), file, git.clone())
    }

    fn root(&self, workspace: &WorkspaceId) -> PathBuf {
        Workspaces::new(&self.state_dir, self.path.clone()).root(workspace)
    }
}

// `answer`, its file said to be too large when it wouldn't fit one line, whatever its id.
fn fitted(mut answer: DiffAnswer) -> DiffAnswer {
    let envelope = Envelope {
        id: u32::MAX,
        message: Message::DiffAnswer(answer.clone()),
    };
    if encode_line(&envelope) == Err(FrameError::TooLarge)
        && let DiffAnswerKind::File { file, .. } = &mut answer.answer
    {
        file.content = DiffContent::TooLarge;
    }
    answer
}

fn unknown() -> Fault {
    Fault::new(
        code!("request.unknown"),
        "This core doesn't know that request",
    )
}

fn failed() -> Fault {
    Fault::new(
        code!("worktree.failed"),
        "The worktree couldn't be answered for",
    )
}

/// Answers for a chat `c-1` in workspace `w-1` whose first prompt named a repository, with its
/// worktree made and recorded under `dir`; its Trash is `dir/bin`.
#[cfg(test)]
pub(crate) fn with_chat(dir: &Path) -> (Worktrees, WorkspaceId, ChatId, PathBuf) {
    use weacon_protocol::RepoId;

    use crate::git::tests::{chat, git, repo};

    let (state, work) = (dir.join("state"), dir.join("work"));
    let workspace = WorkspaceId::try_from("w-1".to_owned()).unwrap();
    let chat_id = chat("c-1");
    let repo = repo(dir);
    let worktree = crate::chats::chat_folder(&work, &workspace, &chat_id);
    let start = git(dir)
        .chat_worktree(&repo, &chat_id, &worktree)
        .unwrap()
        .start
        .unwrap();
    let root = Workspaces::new(&state, None).root(&workspace);
    let site = RepoId::try_from("site".to_owned()).unwrap();
    Repos::new(workspace.clone(), root.join("git/repos.json"))
        .add(&git(dir), site.clone(), &repo)
        .unwrap();
    ChatRepos::new(workspace.clone(), root.join("git/chats.json"))
        .record(&chat_id, site, start)
        .unwrap();
    let worktrees = Worktrees::new(&state, std::env::var_os("PATH"), Some(work), bin);
    (worktrees, workspace, chat_id, worktree)
}

#[cfg(test)]
// A Trash of the test's own: `bin` in the folder above `work/<workspace>/<chat>`.
fn bin(path: &Path, _: SystemTime) -> io::Result<Trashed> {
    let dir = path.ancestors().nth(3).ok_or(io::ErrorKind::InvalidInput)?;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    let item = bin.join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?);
    std::fs::rename(path, &item)?;
    Ok(Trashed { item, info: None })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::{DiffFile, RequestId, WorktreeAction as Action};
    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, fixture};

    struct Bench {
        dir: TempDir,
        worktrees: Worktrees,
        workspace: WorkspaceId,
        chat: ChatId,
        worktree: PathBuf,
    }

    fn bench() -> Bench {
        let dir = TempDir::new().unwrap();
        let (worktrees, workspace, chat, worktree) = with_chat(&dir);
        Bench {
            dir,
            worktrees,
            workspace,
            chat,
            worktree,
        }
    }

    fn request() -> RequestId {
        RequestId::try_from("r-1".to_owned()).unwrap()
    }

    impl Bench {
        async fn changes(&self) -> Result<Vec<DiffAnswerKind>, Fault> {
            let query = DiffQuery {
                request: request(),
                workspace: self.workspace.clone(),
                chat: self.chat.clone(),
                query: DiffQueryKind::Changes,
                by: None,
            };
            let answers = self.worktrees.diff(query).await?;
            Ok(answers.into_iter().map(|answer| answer.answer).collect())
        }

        async fn state(&self) -> Result<WorktreeAnswerKind, Fault> {
            Ok(self.worktrees.query(state_query(self)).await?.answer)
        }

        async fn act(&self, action: Action) -> Result<WorktreeAnswerKind, Fault> {
            let command = WorktreeCommand {
                request: request(),
                workspace: self.workspace.clone(),
                chat: self.chat.clone(),
                action,
                by: None,
            };
            Ok(self.worktrees.command(command).await?.answer)
        }
    }

    // An agent points the worktree's `.git` file at a repository of its own, whose config runs a
    // command whenever git reads a changed file.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_worktrees_git_file_is_never_followed() {
        let bench = bench();
        let (evil, marker) = (bench.dir.join("evil"), bench.dir.join("ran"));
        let repo = bench.dir.join("repo");
        fixture(
            &bench.dir,
            &bench.dir,
            &[
                "clone",
                "--quiet",
                repo.to_str().unwrap(),
                evil.to_str().unwrap(),
            ],
        );
        fs::write(evil.join(".git/info/attributes"), "* filter=evil\n").unwrap();
        let clean = format!("touch '{}'; cat", marker.display());
        fixture(&bench.dir, &evil, &["config", "filter.evil.clean", &clean]);
        let points = format!("gitdir: {}\n", evil.join(".git").display());
        fs::write(bench.worktree.join(".git"), points).unwrap();
        fs::write(bench.worktree.join("README.md"), "# Changed\n").unwrap();

        let changed = bench.changes().await.unwrap();

        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(bench.state().await.unwrap(), state(false, true, false));
        assert!(!marker.exists(), "the agent's filter ran");
    }

    #[tokio::test]
    async fn a_worktree_its_repository_doesnt_list_is_refused() {
        let bench = bench();
        fixture(
            &bench.dir,
            &bench.dir.join("repo"),
            &[
                "worktree",
                "remove",
                "--force",
                bench.worktree.to_str().unwrap(),
            ],
        );
        fs::create_dir_all(&bench.worktree).unwrap();

        let error = bench.state().await.unwrap_err();

        assert_eq!(code(&error), "worktree.unregistered");
    }

    fn one_line(text: String) -> DiffAnswer {
        use weacon_protocol::{DiffLine, FileChange, Hunk, LineOp};

        let line = DiffLine {
            op: LineOp::Added,
            text,
            no_newline: false,
        };
        let file = DiffFile {
            path: "notes.md".to_owned(),
            change: FileChange::Added,
            content: DiffContent::Text {
                hunks: vec![Hunk {
                    old_start: 0,
                    new_start: 1,
                    lines: vec![line],
                }],
            },
        };
        DiffAnswer {
            request: request(),
            workspace: "w-1".parse().unwrap(),
            chat: chat("c-1"),
            answer: DiffAnswerKind::File { files: 1, file },
        }
    }

    fn content(answer: &DiffAnswer) -> &DiffContent {
        let DiffAnswerKind::File { file, .. } = &answer.answer else {
            panic!("not a file");
        };
        &file.content
    }

    #[test]
    fn an_answer_that_fills_a_frame_without_its_newline_is_too_large() {
        let size = |answer: DiffAnswer| {
            let envelope = Envelope {
                id: 0,
                message: Message::DiffAnswer(answer),
            };
            envelope.to_json().unwrap().len()
        };
        let short = size(one_line(String::new()));
        let filling = one_line("a".repeat(weacon_protocol::MAX_FRAME - short));
        assert_eq!(size(filling.clone()), weacon_protocol::MAX_FRAME);
        let fitting = one_line("a".repeat(weacon_protocol::MAX_FRAME - short - 20));

        assert_eq!(content(&fitted(filling)), &DiffContent::TooLarge);
        assert!(matches!(
            content(&fitted(fitting)),
            DiffContent::Text { .. }
        ));
    }

    #[tokio::test]
    async fn a_drain_waits_for_the_work_under_way_then_refuses_more() {
        let bench = bench();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (worktrees, marked) = (bench.worktrees.clone(), Arc::clone(&done));
        let work = move |_: &Inner| {
            held.recv().map_err(|_| failed())?;
            marked.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        };
        tokio::spawn(async move { worktrees.blocking(work).await });
        while bench.worktrees.inner.workers.available_permits() == WORKERS {
            tokio::task::yield_now().await;
        }

        let draining = bench.worktrees.clone();
        let draining = tokio::spawn(async move { draining.drain().await });
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!draining.is_finished());
        let (queued, query) = (bench.worktrees.clone(), state_query(&bench));
        let queued = tokio::spawn(async move { queued.query(query).await });
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        release.send(()).unwrap();
        draining.await.unwrap();
        let refused = queued.await.unwrap().unwrap_err();
        assert_eq!(code(&refused), "worktree.failed");

        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(code(&bench.state().await.unwrap_err()), "worktree.failed");
    }

    #[tokio::test]
    async fn a_panic_while_a_journal_changed_breaks_no_later_change() {
        let bench = bench();
        let inner = Arc::clone(&bench.worktrees.inner);
        std::thread::spawn(move || {
            let _held = inner.journals.lock().unwrap();
            panic!("poisons the lock");
        })
        .join()
        .unwrap_err();

        assert_eq!(
            bench.act(Action::Remove).await.unwrap(),
            state(true, false, false)
        );
    }

    fn state_query(bench: &Bench) -> WorktreeQuery {
        WorktreeQuery {
            request: request(),
            workspace: bench.workspace.clone(),
            chat: bench.chat.clone(),
            query: WorktreeQueryKind::State,
            by: None,
        }
    }

    // The path of a file that's one of two changed.
    fn path_of_two(kind: &DiffAnswerKind) -> Option<&str> {
        let DiffAnswerKind::File { files: 2, file } = kind else {
            return None;
        };
        Some(file.path.as_str())
    }

    fn state(removed: bool, uncommitted: bool, unpushed: bool) -> WorktreeAnswerKind {
        WorktreeAnswerKind::State {
            removed,
            uncommitted,
            unpushed,
        }
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    #[tokio::test]
    async fn a_chats_changes_come_one_file_at_a_time_or_as_none() {
        let bench = bench();

        assert_eq!(bench.changes().await.unwrap(), [DiffAnswerKind::Unchanged]);
        fs::write(bench.worktree.join("agent.txt"), "mine\n").unwrap();
        fs::write(bench.worktree.join("other.txt"), "more\n").unwrap();

        let changes = bench.changes().await.unwrap();

        assert_eq!(changes.len(), 2);
        let paths: Vec<&str> = changes.iter().filter_map(path_of_two).collect();
        assert_eq!(paths, ["agent.txt", "other.txt"]);
    }

    #[tokio::test]
    async fn a_worktree_says_what_removing_it_would_set_aside() {
        let bench = bench();

        assert_eq!(bench.state().await.unwrap(), state(false, false, false));
        fs::write(bench.worktree.join("agent.txt"), "mine\n").unwrap();
        assert_eq!(bench.state().await.unwrap(), state(false, true, false));
        fixture(&bench.dir, &bench.worktree, &["add", "agent.txt"]);
        fixture(
            &bench.dir,
            &bench.worktree,
            &["commit", "--quiet", "-m", "Agent"],
        );
        assert_eq!(bench.state().await.unwrap(), state(false, false, true));
    }

    #[tokio::test]
    async fn removing_and_restoring_answer_with_where_the_worktree_stands() {
        let bench = bench();

        assert_eq!(
            bench.act(Action::Remove).await.unwrap(),
            state(true, false, false)
        );
        assert!(!bench.worktree.exists());
        assert_eq!(
            bench.act(Action::Remove).await.unwrap(),
            state(true, false, false)
        );
        assert_eq!(bench.state().await.unwrap(), state(true, false, false));

        assert_eq!(
            bench.act(Action::Restore).await.unwrap(),
            state(false, false, false)
        );
        assert!(bench.worktree.join("README.md").exists());
        assert_eq!(
            bench.act(Action::Restore).await.unwrap(),
            state(false, false, false)
        );
    }

    #[tokio::test]
    async fn a_pull_request_needs_somewhere_to_push_and_a_page_to_open_on() {
        let bench = bench();
        let open = || Action::OpenPullRequest {
            title: "Fix the build".to_owned(),
            body: String::new(),
        };

        let error = bench.act(open()).await.unwrap_err();
        assert_eq!(code(&error), "pr.push_failed");

        let origin = bench.dir.join("origin.git");
        fixture(
            &bench.dir,
            &bench.dir,
            &["init", "--quiet", "--bare", origin.to_str().unwrap()],
        );
        let repo = bench.dir.join("repo");
        fixture(
            &bench.dir,
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        let error = bench.act(open()).await.unwrap_err();

        // Pushed, but a remote that isn't on GitHub has no page to open one on.
        assert_eq!(code(&error), "pr.no_page");
        fixture(
            &bench.dir,
            &origin,
            &["rev-parse", "--verify", "--quiet", "refs/heads/weacon/c-1"],
        );
    }

    #[tokio::test]
    async fn a_chat_without_a_worktree_or_a_newer_ask_is_refused() {
        let bench = bench();
        let other = DiffQuery {
            request: request(),
            workspace: bench.workspace.clone(),
            chat: chat("c-2"),
            query: DiffQueryKind::Changes,
            by: None,
        };

        let error = bench.worktrees.diff(other).await.unwrap_err();
        assert_eq!(code(&error), "worktree.none");
        for action in [Action::OpenTerminal, Action::Newer] {
            assert_eq!(
                code(&bench.act(action).await.unwrap_err()),
                "request.unknown"
            );
        }
        let newer = WorktreeQuery {
            request: request(),
            workspace: bench.workspace.clone(),
            chat: bench.chat.clone(),
            query: WorktreeQueryKind::Newer,
            by: None,
        };
        let error = bench.worktrees.query(newer).await.unwrap_err();
        assert_eq!(code(&error), "request.unknown");
    }

    #[test]
    fn a_file_too_large_for_a_frame_is_only_said_to_be() {
        let line = weacon_protocol::DiffLine {
            op: weacon_protocol::LineOp::Added,
            text: "a".repeat(weacon_protocol::MAX_FRAME),
            no_newline: false,
        };
        let file = |lines| DiffFile {
            path: "big.txt".to_owned(),
            change: weacon_protocol::FileChange::Added,
            content: DiffContent::Text {
                hunks: vec![weacon_protocol::Hunk {
                    old_start: 0,
                    new_start: 1,
                    lines,
                }],
            },
        };
        let answer = |file| DiffAnswer {
            request: request(),
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            chat: chat("c-1"),
            answer: DiffAnswerKind::File { files: 1, file },
        };

        let fitted_big = fitted(answer(file(vec![line])));
        let small = answer(file(Vec::new()));

        let DiffAnswerKind::File { file: big, .. } = fitted_big.answer else {
            panic!("not a file");
        };
        assert_eq!(big.content, DiffContent::TooLarge);
        assert_eq!(fitted(small.clone()), small);
    }
}
