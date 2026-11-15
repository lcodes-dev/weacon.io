//! The code map each run's MCP calls answer from: the core's stored map of the run's
//! repository, only when the run's workspace registered that repository, kept current by a
//! watch of each live run's worktree.

mod hook;
#[cfg(test)]
pub(crate) mod test_support;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};
use tokio::time::MissedTickBehavior;
use weacon_codemap::{Index, assemble};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{McpCall, RepoId, WorkspaceId, code};

use super::grants::{Grant, Grants};
use super::tools::{CHURN_COMMITS, NO_MAP, answer_from, budget, found};
use crate::app::WarnOnce;
use crate::chats::Places;
use crate::codemap::{Repo, Watch, locate, off_runtime, repo_map, watch, worktree_index};
use crate::db::{Db, Hit, Looking};
use crate::git::Git;
use crate::git::repos::Repo as Registered;
use crate::git::state::Stores as GitStores;
use crate::workspaces::Workspaces;

/// The longest a call waits for its map; a first read of a large repository can take longer.
const LIMIT: Duration = Duration::from_secs(30);
const TOO_SLOW: &str = "The code map is still being read: try again shortly.\n";
/// How often each watched worktree is checked for changes.
const POLL: Duration = Duration::from_secs(5);
/// How many calls at once, across every run, may read a worktree no watch has read yet.
const READS_AT_ONCE: usize = 2;

/// The stores the MCP's answers come from, opened when its watches start, once the daemon
/// holds its state folder, so a second daemon that can't start changes nothing.
#[derive(Clone, Debug)]
pub(crate) struct Maps {
    state_dir: PathBuf,
    path: Option<OsString>,
    stores: Arc<OnceCell<Option<Stores>>>,
    limit: Duration,
}

#[derive(Clone, Debug)]
struct Stores {
    db: Db,
    git: Git,
    workspaces: Workspaces,
    /// The current index of each watched worktree, by its workspace and paths.
    live: Arc<Mutex<HashMap<Watched, Arc<Index>>>>,
    reads: Arc<Semaphore>,
}

/// A read of a map that didn't end within its limit.
#[derive(Debug)]
struct TooSlow;

/// A live run's worktree, in a repository its workspace registered, by its canonical paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Watched {
    workspace: WorkspaceId,
    main: PathBuf,
    worktree: PathBuf,
}

impl Maps {
    /// The maps of `state_dir`, with git found on the `PATH` in `places`.
    pub(crate) fn new(state_dir: &Path, places: &Places) -> Self {
        Self {
            state_dir: state_dir.to_owned(),
            path: places.path.clone(),
            stores: Arc::default(),
            limit: LIMIT,
        }
    }

    /// Maps whose stores can't be opened, which answer that there's no map.
    #[cfg(test)]
    pub(crate) fn off() -> Self {
        Self::new(Path::new("/nonexistent/weacon"), &Places::default())
    }

    /// The answer to `call` from the map of `grant`'s folder, whose repository its workspace
    /// must have registered; "no map" otherwise, or when the map can't be read. `permit` is
    /// held until the read ends, even after the call stops waiting for it.
    pub(crate) async fn answer(
        &self,
        call: &McpCall,
        grant: &Grant,
        permit: OwnedSemaphorePermit,
        log: &Logger,
    ) -> String {
        let place = (grant.workspace.clone(), grant.folder.clone());
        match self.stored(call, place, permit, log).await {
            Ok(Some(answer)) => answer,
            Ok(None) => NO_MAP.to_owned(),
            Err(TooSlow) => TOO_SLOW.to_owned(),
        }
    }

    /// [`Self::answer`]'s answer from the map of the folder in `place`'s workspace when there
    /// is one; none when there isn't, and [`TooSlow`] once the limit passes.
    async fn stored(
        &self,
        call: &McpCall,
        (workspace, folder): (WorkspaceId, PathBuf),
        permit: OwnedSemaphorePermit,
        log: &Logger,
    ) -> Result<Option<String>, TooSlow> {
        let Some(stores) = self.stores(log).await else {
            return Ok(None);
        };
        let (call, reading_log) = (call.clone(), log.clone());
        // Its own task, so a read the limit cuts off still finishes and is stored for the next.
        let reading = tokio::spawn(async move {
            let _permit = permit;
            let answer = stores.answer(&call, workspace, folder).await;
            answer
                .inspect_err(|fault| {
                    reading_log.warn("mcp map failed", &[("err", Field::error(fault))]);
                })
                .ok()
                .flatten()
        });
        match tokio::time::timeout(self.limit, reading).await {
            Err(_) => Err(TooSlow),
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(error)) => {
                let fault = failed().because(error);
                log.warn("mcp map failed", &[("err", Field::error(&fault))]);
                Ok(None)
            }
        }
    }

    /// Keeps the index of each live run's worktree in `grants` current, checking them every
    /// [`POLL`]; it runs until dropped.
    #[expect(
        clippy::infinite_loop,
        reason = "it runs until the server's task is dropped"
    )]
    pub(crate) async fn keep_current(&self, grants: &Grants, log: &Logger) {
        let Some(stores) = self.stores(log).await else {
            return;
        };
        let (mut watches, mut listing) = (HashMap::new(), WarnOnce::default());
        let mut poll = tokio::time::interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            poll.tick().await;
            let (found, failed) = stores.watched(grants.folders()).await;
            listing.check(failed.map_or(Ok(()), Err), log, "map list failed");
            stores.check(&mut watches, found, log).await;
        }
    }

    /// What search finds for `words` among the items of each repository `workspace`
    /// registered, at its main worktree's commit, best first in each; one that can't be read is
    /// left out. A base read past the limit goes on, so the next search finds it stored.
    pub(crate) async fn symbols(
        &self,
        workspace: &WorkspaceId,
        words: &str,
        log: &Logger,
    ) -> Result<Vec<(RepoId, Vec<Hit>)>, Fault> {
        let stores = self.stores(log).await.ok_or_else(failed)?;
        // One of the reads at once for the whole search, taken here so searches never pile up.
        let read = Arc::clone(&stores.reads).try_acquire_owned();
        let read = read.map_err(|_| still_reading())?;
        let (workspace, words, log) = (workspace.clone(), words.to_owned(), log.clone());
        let reading = tokio::spawn(async move {
            let _read = read;
            stores.symbols(workspace, &words, &log).await
        });
        match tokio::time::timeout(self.limit, reading).await {
            Err(_) => Err(still_reading()),
            Ok(Ok(found)) => found,
            Ok(Err(error)) => Err(failed().because(error)),
        }
    }

    /// The stores, opened at the first use; none, logged once, when either can't be.
    async fn stores(&self, log: &Logger) -> Option<Stores> {
        let opened = self.stores.get_or_init(|| async {
            open(&self.state_dir, self.path.clone())
                .await
                .inspect_err(|fault| log.warn("mcp maps off", &[("err", Field::error(fault))]))
                .ok()
        });
        opened.await.clone()
    }
}

async fn open(state_dir: &Path, path: Option<OsString>) -> Result<Stores, Fault> {
    let db = Db::open(state_dir).await?;
    let git = Git::open(state_dir, path.as_deref())?;
    Ok(Stores {
        db,
        git,
        workspaces: Workspaces::new(state_dir, path),
        live: Arc::default(),
        reads: Arc::new(Semaphore::new(READS_AT_ONCE)),
    })
}

impl Stores {
    /// What [`Maps::symbols`] finds, reading a repository's base when it must.
    async fn symbols(
        &self,
        workspace: WorkspaceId,
        words: &str,
        log: &Logger,
    ) -> Result<Vec<(RepoId, Vec<Hit>)>, Fault> {
        let (listing, asked) = (self.clone(), workspace.clone());
        let repos = off_runtime(move || listing.mains(&asked)).await?;
        let store = self.db.workspace(&workspace);
        let mut found = Vec::new();
        for (repo, main) in repos {
            let map = repo_map(&store, &self.git, main);
            match map.search_head(words, Looking::Items).await {
                Ok(hits) => found.push((repo, hits)),
                Err(fault) => {
                    let about = [
                        ("repo", repo.as_str().into()),
                        ("err", Field::error(&fault)),
                    ];
                    log.warn("symbols repo failed", &about);
                }
            }
        }
        Ok(found)
    }

    /// The answer to `call` from the map of `folder`, with the churn of its worktree for
    /// `hotspots`; none when [`Self::index`] finds no map. A search looks through the
    /// worktree's stored layer.
    async fn answer(
        &self,
        call: &McpCall,
        workspace: WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<String>, Fault> {
        let Some((watched, index)) = self.index(workspace, folder).await? else {
            return Ok(None);
        };
        if let McpCall::Search(args) = call {
            let store = self.db.workspace(&watched.workspace);
            let map = repo_map(&store, &self.git, watched.main);
            let hits = map.search(Some(&watched.worktree), &args.words, Looking::Everything);
            return Ok(Some(found(&hits.await?, budget(args.budget))));
        }
        let churn = if matches!(call, McpCall::Hotspots(_)) {
            let (git, worktree) = (self.git.clone(), watched.worktree);
            off_runtime(move || git.churn(&worktree, CHURN_COMMITS)).await?
        } else {
            Vec::new()
        };
        Ok(Some(answer_from(call, &index, &churn)))
    }

    /// The index of `folder` and its worktree, or none when it isn't in a repository
    /// `workspace` registered: its watch's index when it has one, else read now, which stores it.
    async fn index(
        &self,
        workspace: WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<(Watched, Arc<Index>)>, Fault> {
        let stores = self.clone();
        let Some(watched) = off_runtime(move || stores.registered(workspace, &folder)).await?
        else {
            return Ok(None);
        };
        let live = self.live().get(&watched).map(Arc::clone);
        if let Some(index) = live {
            return Ok(Some((watched, index)));
        }
        let _read = self
            .reads
            .acquire()
            .await
            .map_err(|error| failed().because(error))?;
        let store = self.db.workspace(&watched.workspace);
        let main = watched.main.clone();
        let index = worktree_index(&store, &self.git, main, &watched.worktree).await?;
        Ok(Some((watched, Arc::new(index))))
    }

    /// `folder`'s worktree, when its repository is one `workspace` registered by its main
    /// worktree or this one; a folder in no repository isn't. It blocks.
    fn registered(&self, workspace: WorkspaceId, folder: &Path) -> Result<Option<Watched>, Fault> {
        let Ok((main, worktree)) = locate(&self.git, folder) else {
            return Ok(None);
        };
        let repos = GitStores::new(&self.workspaces, &workspace)
            .repos()
            .list()?;
        let listed = |repo: &Registered| repo.path == main || repo.path == worktree;
        Ok(repos.iter().any(listed).then_some(Watched {
            workspace,
            main,
            worktree,
        }))
    }

    /// The id and main worktree of each repository `workspace` registered that is one here,
    /// once each; it blocks.
    fn mains(&self, workspace: &WorkspaceId) -> Result<Vec<(RepoId, PathBuf)>, Fault> {
        let repos = GitStores::new(&self.workspaces, workspace).repos().list()?;
        let mut mains: Vec<(RepoId, PathBuf)> = Vec::new();
        for repo in repos {
            let Ok((main, _)) = locate(&self.git, &repo.path) else {
                continue;
            };
            if !mains.iter().any(|(_, known)| *known == main) {
                mains.push((repo.id, main));
            }
        }
        Ok(mains)
    }

    /// The worktrees of `folders`, each a live run's folder in its own workspace, once each,
    /// and the first failure to find one.
    async fn watched(&self, folders: Vec<(WorkspaceId, PathBuf)>) -> (Vec<Watched>, Option<Fault>) {
        let stores = self.clone();
        let found = off_runtime(move || Ok(stores.found(folders)));
        found
            .await
            .unwrap_or_else(|fault| (Vec::new(), Some(fault)))
    }

    // It blocks.
    fn found(&self, folders: Vec<(WorkspaceId, PathBuf)>) -> (Vec<Watched>, Option<Fault>) {
        let mut found: Vec<Watched> = Vec::new();
        let mut failed = None;
        for (workspace, folder) in folders {
            match self.registered(workspace, &folder) {
                Ok(Some(watched)) if !found.contains(&watched) => found.push(watched),
                Ok(_) => {}
                Err(fault) => failed = failed.or(Some(fault)),
            }
        }
        (found, failed)
    }

    /// Checks each of `found`, keeping a watch for each, and forgets the watches and indexes of
    /// worktrees no live run is in.
    async fn check(
        &self,
        watches: &mut HashMap<Watched, Watch<Repo>>,
        found: Vec<Watched>,
        log: &Logger,
    ) {
        watches.retain(|watched, _| found.contains(watched));
        self.live().retain(|watched, _| found.contains(watched));
        for watched in found {
            let watch = watches.entry(watched.clone()).or_insert_with(|| {
                let workspace = self.db.workspace(&watched.workspace);
                watch(
                    &workspace,
                    &self.git,
                    watched.main.clone(),
                    watched.worktree.clone(),
                )
            });
            let log = log.child(&[("workspace_id", watched.workspace.as_str().into())]);
            let Some(facts) = watch.check(&log).await else {
                continue;
            };
            match off_runtime(move || Ok(assemble(&facts))).await {
                Ok(index) => drop(self.live().insert(watched, Arc::new(index))),
                Err(fault) => log.warn("map update failed", &[("err", Field::error(&fault))]),
            }
        }
    }

    // Only the map is guarded, so a poisoned lock still holds a usable one.
    fn live(&self) -> MutexGuard<'_, HashMap<Watched, Arc<Index>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn still_reading() -> Fault {
    Fault::new(
        code!("symbols.still_reading"),
        "The code map is still being read",
    )
}

fn failed() -> Fault {
    Fault::new(code!("mcp.map_failed"), "The code map couldn't be read")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::sync::Semaphore;
    use weacon_protocol::MapArgs;
    use weacon_testing::{TempDir, recording_logger};

    use super::test_support::{crate_repo, setup, workspace};
    use super::*;
    use crate::git::fixture;

    fn map() -> McpCall {
        McpCall::Map(MapArgs::default())
    }

    fn grant(workspace: &WorkspaceId, folder: &Path) -> Grant {
        let run = weacon_protocol::RunId::try_from("run-1".to_owned()).unwrap();
        Grant::new(workspace.clone(), run, folder.to_owned())
    }

    fn permit() -> OwnedSemaphorePermit {
        Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap()
    }

    #[tokio::test]
    async fn a_registered_repository_answers_from_its_stored_map() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let one = workspace("one");
        let main = crate_repo(&dir, "marker");
        setup.register(&one, "site", &main);
        let (log, records) = recording_logger();

        let answer = setup
            .maps
            .answer(&map(), &grant(&one, &main.join("src")), permit(), &log)
            .await;

        assert!(answer.contains("fn marker::run"), "{answer}");
        assert!(setup.stored(&one, &main).await);
        assert!(records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn hotspots_count_the_commits_of_the_runs_own_worktree_from_its_root() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let one = workspace("one");
        let main = crate_repo(&dir, "marker");
        setup.register(&one, "site", &main);
        let chat = dir.join("chat");
        let add = [
            "worktree",
            "add",
            "--quiet",
            "-b",
            "chat",
            chat.to_str().unwrap(),
        ];
        fixture(&dir, &main, &add);
        fs::write(chat.join("src/lib.rs"), "pub fn run() { }\n").unwrap();
        fixture(&dir, &chat, &["commit", "--quiet", "--all", "-m", "Change"]);
        let (log, records) = recording_logger();
        let hotspots = McpCall::Hotspots(MapArgs::default());
        let ask = async |folder: &Path| {
            let grant = grant(&one, folder);
            setup.maps.answer(&hotspots, &grant, permit(), &log).await
        };

        let title = "Hotspots: files that change often and that much depends on.\n";
        let line = |commits| format!("{title}- src/lib.rs  {commits} commits, 0 edges in\n");
        assert_eq!(ask(&chat.join("src")).await, line(2));
        assert_eq!(ask(&main).await, line(1));
        assert!(records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_repository_the_workspace_never_registered_has_no_map_and_stores_none() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let (one, two) = (workspace("one"), workspace("two"));
        let main = crate_repo(&dir, "one");
        setup.register(&one, "site", &main);
        let (log, records) = recording_logger();
        let maps = &setup.maps;

        let elsewhere = maps
            .answer(&map(), &grant(&two, &main), permit(), &log)
            .await;
        let state = dir.join("state");
        let no_repo = maps
            .answer(&map(), &grant(&one, &state), permit(), &log)
            .await;

        assert_eq!((elsewhere.as_str(), no_repo.as_str()), (NO_MAP, NO_MAP));
        assert!(!setup.stored(&two, &main).await);
        assert!(records.lock().unwrap().is_empty());
    }

    fn search(words: &str) -> McpCall {
        McpCall::Search(weacon_protocol::SearchArgs {
            words: words.to_owned(),
            budget: None,
        })
    }

    #[tokio::test]
    async fn a_search_looks_through_the_runs_worktree_as_it_is_now() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let (one, two) = (workspace("one"), workspace("two"));
        let main = crate_repo(&dir, "one");
        setup.register(&one, "site", &main);
        fs::write(main.join("src/lib.rs"), "\npub fn walk() {}\n").unwrap();
        let (log, records) = recording_logger();
        let ask = async |call: McpCall, workspace: &WorkspaceId| {
            let grant = grant(workspace, &main);
            setup.maps.answer(&call, &grant, permit(), &log).await
        };

        assert_eq!(
            ask(search("walk"), &one).await,
            "Found 1:\n- fn one::walk  src/lib.rs:2\n"
        );
        assert_eq!(ask(search("run"), &one).await, "Found 0:\n");
        assert_eq!(ask(search("walk"), &two).await, NO_MAP);
        assert!(records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn symbols_are_searched_at_the_commit_of_each_repository_the_workspace_registered() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let (one, two) = (workspace("one"), workspace("two"));
        let (alpha, beta, empty) = (dir.join("a"), dir.join("b"), dir.join("empty"));
        let (alpha, beta) = (crate_repo(&alpha, "alpha"), crate_repo(&beta, "beta"));
        fs::write(alpha.join("src/lib.rs"), "pub fn walk() {}\n").unwrap();
        fs::write(alpha.join("README.md"), "How to run.\n").unwrap();
        fs::create_dir_all(&empty).unwrap();
        fixture(&dir, &empty, &["init", "--quiet"]);
        let linked = dir.join("linked");
        let linked_arg = linked.to_str().unwrap();
        fixture(
            &dir,
            &alpha,
            &["worktree", "add", "--quiet", "-b", "w", linked_arg],
        );
        for (id, repo) in [
            ("a", &alpha),
            ("again", &linked),
            ("b", &beta),
            ("empty", &empty),
        ] {
            setup.register(&one, id, repo);
        }
        setup.register(&two, "b", &beta);
        let (log, records) = recording_logger();

        let found = setup.maps.symbols(&one, "run", &log).await.unwrap();

        let names = |found: &[(RepoId, Vec<Hit>)]| -> Vec<(String, Vec<String>)> {
            let names = |hits: &[Hit]| hits.iter().map(|hit| hit.name.clone()).collect();
            let repo = |(repo, hits): &(RepoId, Vec<Hit>)| (repo.as_str().to_owned(), names(hits));
            found.iter().map(repo).collect()
        };
        let run = |name: &str| vec![name.to_owned()];
        assert_eq!(
            names(&found),
            [
                ("a".to_owned(), run("alpha::run")),
                ("b".to_owned(), run("beta::run"))
            ]
        );
        assert_eq!(weacon_testing::messages(&records), ["symbols repo failed"]);
        let theirs = setup.maps.symbols(&two, "run", &log).await.unwrap();
        assert_eq!(names(&theirs), [("b".to_owned(), run("beta::run"))]);
        let off = Maps::off().symbols(&one, "run", &log).await;
        assert_eq!(off.unwrap_err().code().as_str(), "mcp.map_failed");
    }

    #[tokio::test]
    async fn a_symbols_search_past_the_limit_or_with_no_read_free_is_still_reading() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let one = workspace("one");
        setup.register(&one, "site", &crate_repo(&dir, "one"));
        let (log, _records) = recording_logger();

        let mut slow = setup.maps.clone();
        slow.limit = Duration::ZERO;
        let late = slow.symbols(&one, "run", &log).await.unwrap_err();
        assert_eq!(late.code().as_str(), "symbols.still_reading");
        let stores = setup.maps.stores(&log).await.unwrap();
        let held = Arc::clone(&stores.reads)
            .acquire_many_owned(2)
            .await
            .unwrap();
        let busy = setup.maps.symbols(&one, "run", &log).await.unwrap_err();
        assert_eq!(busy.code().as_str(), "symbols.still_reading");
        drop(held);
        let found = setup.maps.symbols(&one, "run", &log).await.unwrap();
        assert_eq!(found.len(), 1);
    }

    #[tokio::test]
    async fn a_read_past_the_limit_keeps_its_slot_and_a_failed_one_is_logged() {
        let dir = TempDir::new().unwrap();
        let mut setup = setup(&dir).await;
        let one = workspace("one");
        let main = crate_repo(&dir, "one");
        let (log, records) = recording_logger();
        let calls = Arc::new(Semaphore::new(1));
        setup.maps.limit = Duration::ZERO;
        let list = setup.workspaces.root(&one).join("git/repos.json");
        fs::create_dir_all(list).unwrap();

        let slot = Arc::clone(&calls).try_acquire_owned().unwrap();
        let late = setup
            .maps
            .answer(&map(), &grant(&one, &main), slot, &log)
            .await;
        assert_eq!(calls.available_permits(), 0);
        drop(calls.acquire().await.unwrap());

        assert_eq!(late, TOO_SLOW);
        assert_eq!(weacon_testing::messages(&records), ["mcp map failed"]);
    }

    #[tokio::test]
    async fn a_live_runs_worktree_is_watched_until_its_run_ends_or_its_repository_goes() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let one = workspace("one");
        let main = crate_repo(&dir, "one");
        setup.register(&one, "site", &main);
        let (log, _records) = recording_logger();
        let stores = setup.maps.stores(&log).await.unwrap();
        let mut watches = HashMap::new();
        let ask = async || {
            setup
                .maps
                .answer(&map(), &grant(&one, &main), permit(), &log)
                .await
        };
        let runs = vec![(one.clone(), main.join("src")), (one.clone(), main.clone())];
        let check = async |watches: &mut HashMap<Watched, Watch<Repo>>, runs: Vec<_>| {
            let (found, failed) = stores.watched(runs).await;
            assert!(failed.is_none());
            stores.check(watches, found, &log).await;
        };

        check(&mut watches, runs.clone()).await;
        assert_eq!(watches.len(), 1);
        fs::write(main.join("src/lib.rs"), "pub fn walk() {}\n").unwrap();
        // Served from the watch, which hasn't looked again yet.
        assert!(ask().await.contains("fn one::run"));
        check(&mut watches, runs.clone()).await;
        assert!(ask().await.contains("fn one::walk"));

        check(&mut watches, Vec::new()).await;
        assert!(watches.is_empty() && stores.live().is_empty());
        check(&mut watches, runs.clone()).await;
        fs::remove_file(setup.workspaces.root(&one).join("git/repos.json")).unwrap();
        check(&mut watches, runs).await;
        assert!(watches.is_empty() && stores.live().is_empty());
        assert_eq!(ask().await, NO_MAP);
    }

    #[tokio::test]
    async fn only_a_runs_own_worktree_is_watched_whoever_shares_its_repository() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let (one, two) = (workspace("one"), workspace("two"));
        let main = crate_repo(&dir, "one");
        setup.register(&one, "site", &main);
        let chat = dir.join("two-chat");
        let chat_arg = chat.to_str().unwrap();
        fixture(
            &dir,
            &main,
            &["worktree", "add", "--quiet", "-b", "chat", chat_arg],
        );
        let chat = chat.canonicalize().unwrap();
        // Registered by the linked worktree's own path.
        setup.register(&two, "site", &chat);
        let (log, _records) = recording_logger();
        let stores = setup.maps.stores(&log).await.unwrap();

        let own = dir.join("one-chat");
        let own_arg = own.to_str().unwrap();
        fixture(
            &dir,
            &main,
            &["worktree", "add", "--quiet", "-b", "own", own_arg],
        );
        let own = own.canonicalize().unwrap();
        let runs = vec![(one.clone(), main.clone()), (one.clone(), main.join("src"))];
        let (found, _) = stores.watched(runs).await;
        let (in_own, _) = stores.watched(vec![(one.clone(), own.clone())]).await;
        let (linked, _) = stores.watched(vec![(two.clone(), chat.clone())]).await;

        let trees = |found: &[Watched]| -> Vec<(WorkspaceId, PathBuf)> {
            let tree = |watched: &Watched| (watched.workspace.clone(), watched.worktree.clone());
            found.iter().map(tree).collect()
        };
        assert_eq!(trees(&found), [(one.clone(), main.clone())]);
        assert_eq!(trees(&in_own), [(one, own)]);
        assert_eq!(trees(&linked), [(two, chat)]);
        assert!(linked.iter().all(|watched| watched.main == main));
    }

    async fn until_watched(stores: &Stores) {
        while stores.live().is_empty() {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn keeping_current_watches_a_live_run_from_its_first_check() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let one = workspace("one");
        let main = crate_repo(&dir, "one");
        setup.register(&one, "site", &main);
        let (log, _records) = recording_logger();
        let stores = setup.maps.stores(&log).await.unwrap();
        let grants = Grants::default();
        let (_token, _revoke) = grants.issue(grant(&one, &main)).unwrap();
        let maps = setup.maps.clone();
        let keeping = tokio::spawn(async move { maps.keep_current(&grants, &log).await });

        let watched = tokio::time::timeout(Duration::from_secs(30), until_watched(&stores));

        watched.await.unwrap();
        keeping.abort();
    }

    #[tokio::test]
    async fn stores_that_cant_be_opened_turn_the_maps_off_once() {
        let dir = TempDir::new().unwrap();
        let places = Places {
            path: Some(dir.join("empty").into()),
            ..Places::default()
        };
        let maps = Maps::new(&dir, &places);
        let (log, records) = recording_logger();

        for _ in 0..2 {
            let answer = maps
                .answer(&map(), &grant(&workspace("one"), &dir), permit(), &log)
                .await;
            assert_eq!(answer, NO_MAP);
        }
        assert_eq!(weacon_testing::messages(&records), ["mcp maps off"]);
    }
}
