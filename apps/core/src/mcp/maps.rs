//! The code map each run's MCP calls answer from: the core's stored map of the run's
//! repository, only when the run's workspace registered that repository, kept current by a
//! watch of each live run's worktree.

#[cfg(test)]
pub(crate) mod test_support;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};
use tokio::time::MissedTickBehavior;
use weacon_codemap::{Facts, Index, assemble};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{CommitId, McpCall, WorkspaceId, code};

use super::grants::{Grant, Grants};
use super::tools::{CHURN_COMMITS, NO_MAP, answer_from};
use crate::app::WarnOnce;
use crate::chats::Places;
use crate::codemap::{Move, Repo, Watch, locate, off_runtime, watch, worktree_index};
use crate::db::Db;
use crate::git::Git;
use crate::git::repos::Repo as Registered;
use crate::git::state::Stores as GitStores;
use crate::proposals::{self, Proposals};
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
    proposals: Option<Proposals>,
}

#[derive(Clone, Debug)]
struct Stores {
    db: Db,
    git: Git,
    workspaces: Workspaces,
    /// The current index of each watched worktree, by its workspace and paths.
    live: Arc<Mutex<HashMap<Watched, Arc<Index>>>>,
    reads: Arc<Semaphore>,
    /// Where what a move of a base did to docs' anchors is proposed, and the commit each
    /// repository's base last moved to, by its workspace and main worktree.
    proposals: Option<Proposals>,
    moves: Arc<Mutex<HashMap<(WorkspaceId, PathBuf), Checked>>>,
}

/// The commit a repository's base last moved to, and the base before it while checking that
/// move failed, so the next move is checked from there.
#[derive(Debug)]
struct Checked {
    commit: CommitId,
    /// How many moves of this repository came before this one.
    seq: u64,
    /// The base the oldest failed check started from, and that check's `seq`.
    failed: Option<(u64, Vec<Facts>)>,
}

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
            proposals: None,
        }
    }

    /// These maps, proposing what each move of a watched repository's base did to its
    /// workspace's docs through `proposals`.
    pub(crate) fn proposing(self, proposals: Proposals) -> Self {
        Self {
            proposals: Some(proposals),
            ..self
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
        let Some(stores) = self.stores(log).await else {
            return NO_MAP.to_owned();
        };
        let (workspace, folder) = (grant.workspace.clone(), grant.folder.clone());
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
            Err(_) => TOO_SLOW.to_owned(),
            Ok(Ok(Some(answer))) => answer,
            Ok(Ok(None)) => NO_MAP.to_owned(),
            Ok(Err(error)) => {
                let fault = failed().because(error);
                log.warn("mcp map failed", &[("err", Field::error(&fault))]);
                NO_MAP.to_owned()
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

    /// The stores, opened at the first use; none, logged once, when either can't be.
    async fn stores(&self, log: &Logger) -> Option<Stores> {
        let opened = self.stores.get_or_init(|| async {
            open(&self.state_dir, self.path.clone(), self.proposals.clone())
                .await
                .inspect_err(|fault| log.warn("mcp maps off", &[("err", Field::error(fault))]))
                .ok()
        });
        opened.await.clone()
    }
}

async fn open(
    state_dir: &Path,
    path: Option<OsString>,
    proposals: Option<Proposals>,
) -> Result<Stores, Fault> {
    let db = Db::open(state_dir).await?;
    let git = Git::open(state_dir, path.as_deref())?;
    Ok(Stores {
        db,
        git,
        workspaces: Workspaces::new(state_dir, path),
        live: Arc::default(),
        reads: Arc::new(Semaphore::new(READS_AT_ONCE)),
        proposals,
        moves: Arc::default(),
    })
}

impl Stores {
    /// The answer to `call` from the map of `folder`, with the churn of its worktree for
    /// `hotspots`; none when [`Self::index`] finds no map.
    async fn answer(
        &self,
        call: &McpCall,
        workspace: WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<String>, Fault> {
        let Some((index, worktree)) = self.index(workspace, folder).await? else {
            return Ok(None);
        };
        let churn = if matches!(call, McpCall::Hotspots(_)) {
            let git = self.git.clone();
            off_runtime(move || git.churn(&worktree, CHURN_COMMITS)).await?
        } else {
            Vec::new()
        };
        Ok(Some(answer_from(call, &index, &churn)))
    }

    /// The index of `folder` and its worktree, or none when it isn't in a repository
    /// `workspace` registered: its watch's index when it has one, else read now.
    async fn index(
        &self,
        workspace: WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<(Arc<Index>, PathBuf)>, Fault> {
        let stores = self.clone();
        let Some(watched) = off_runtime(move || stores.registered(workspace, &folder)).await?
        else {
            return Ok(None);
        };
        if let Some(index) = self.live().get(&watched) {
            return Ok(Some((Arc::clone(index), watched.worktree)));
        }
        let _read = self
            .reads
            .acquire()
            .await
            .map_err(|error| failed().because(error))?;
        let store = self.db.workspace(&watched.workspace);
        let index = worktree_index(&store, &self.git, watched.main, &watched.worktree).await?;
        Ok(Some((Arc::new(index), watched.worktree)))
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
            let facts = watch.check(&log).await;
            if let Some(moved) = watch.take_move() {
                self.moved(&watched, moved, &log);
            }
            let Some(facts) = facts else {
                continue;
            };
            match off_runtime(move || Ok(assemble(&facts))).await {
                Ok(index) => drop(self.live().insert(watched, Arc::new(index))),
                Err(fault) => log.warn("map update failed", &[("err", Field::error(&fault))]),
            }
        }
    }

    /// Proposes, on a task of its own, what `moved` did to the anchors of `watched`'s
    /// workspace's docs, once for each commit a repository's base moves to; a move whose
    /// check failed is checked again with the next.
    fn moved(&self, watched: &Watched, mut moved: Move, log: &Logger) {
        let Some(proposals) = self.proposals.clone() else {
            return;
        };
        let key = (watched.workspace.clone(), watched.main.clone());
        let mut seen = self.moves.lock().unwrap_or_else(PoisonError::into_inner);
        let earlier = seen.get(&key);
        if earlier.is_some_and(|earlier| earlier.commit == moved.commit) {
            return;
        }
        if let Some((_, was)) = earlier.and_then(|earlier| earlier.failed.clone()) {
            moved.was = was;
        }
        let seq = earlier.map_or(0, |earlier| earlier.seq.saturating_add(1));
        let checked = Checked {
            commit: moved.commit.clone(),
            seq,
            failed: None,
        };
        seen.insert(key.clone(), checked);
        drop(seen);
        let checks = Arc::clone(&self.moves);
        tokio::spawn(check_move(
            proposals,
            (key, seq),
            moved,
            checks,
            log.clone(),
        ));
    }

    // Only the map is guarded, so a poisoned lock still holds a usable one.
    fn live(&self) -> MutexGuard<'_, HashMap<Watched, Arc<Index>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Proposes what `moved`, the move numbered `seq`, did to the docs of the workspace in `key`,
/// and when that fails keeps the base it started from in `checks`, for the next move.
async fn check_move(
    proposals: Proposals,
    (key, seq): ((WorkspaceId, PathBuf), u64),
    moved: Move,
    checks: Arc<Mutex<HashMap<(WorkspaceId, PathBuf), Checked>>>,
    log: Logger,
) {
    let was = moved.was.clone();
    let assembled = move || Ok((assemble(&moved.was), assemble(&moved.now), moved.commit));
    let checked = async {
        let (was, now, commit) = off_runtime(assembled).await?;
        proposals::moved(&proposals, &key.0, &was, &now, &commit).await
    };
    let fault = match checked.await {
        Ok(docs) => return log.info("anchors checked", &[("docs", docs.into())]),
        Err(fault) => fault,
    };
    log.warn("anchors check failed", &[("err", Field::error(&fault))]);
    // Checks of later moves may fail first; the oldest base failed from is kept, so the next
    // move covers what each missed.
    let mut seen = checks.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(checked) = seen.get_mut(&key)
        && checked.failed.as_ref().is_none_or(|(kept, _)| seq < *kept)
    {
        checked.failed = Some((seq, was));
    }
}

fn failed() -> Fault {
    Fault::new(code!("mcp.map_failed"), "The code map couldn't be read")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::sync::Semaphore;
    use weacon_protocol::MapArgs;
    use weacon_testing::{TempDir, messages, recording_logger};

    use super::test_support::{crate_repo, setup, workspace};
    use super::*;
    use crate::git::fixture;
    use crate::proposals::fake::Fake;

    fn map() -> McpCall {
        McpCall::Map(MapArgs::default())
    }

    fn grant(workspace: &WorkspaceId, folder: &Path) -> Grant {
        let run = weacon_protocol::RunId::try_from("run-1".to_owned()).unwrap();
        let claude = weacon_protocol::AgentKind::Claude;
        Grant::new(workspace.clone(), run, claude, folder.to_owned())
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
        assert_eq!(messages(&records), ["mcp map failed"]);
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
        assert_eq!(messages(&records), ["mcp maps off"]);
    }

    fn facts(files: &[(&str, &str)]) -> Vec<Facts> {
        let mut reader = weacon_codemap::Reader::new().unwrap();
        files
            .iter()
            .filter_map(|(path, text)| {
                let file = weacon_codemap::SourceFile {
                    path: (*path).to_owned(),
                    text: (*text).to_owned(),
                };
                reader.facts(&file).unwrap()
            })
            .collect()
    }

    fn pinned_doc(pin: &str) -> weacon_protocol::DocRecord {
        weacon_protocol::DocRecord {
            id: "d-1".parse().unwrap(),
            version: 1,
            content: weacon_protocol::DocContent {
                title: "Plan".to_owned(),
                body: "[go](sym:a::go)\n".to_owned(),
                anchors: Some(weacon_protocol::DocAnchors {
                    commit: "a".repeat(40).parse().unwrap(),
                    pins: vec![pin.parse().unwrap()],
                }),
            },
        }
    }

    #[tokio::test]
    async fn each_move_of_a_repositorys_base_is_checked_once() {
        let dir = TempDir::new().unwrap();
        let fake = Fake::default();
        let path = std::env::var_os("PATH");
        let stores = open(&dir, path.clone(), Some(fake.connect()))
            .await
            .unwrap();
        let manifest = ("Cargo.toml", "[package]\nname = \"a\"\n");
        let was = facts(&[manifest, ("src/lib.rs", "pub fn go() {}\n")]);
        let now = facts(&[manifest, ("src/lib.rs", "pub fn went() {}\n")]);
        let index = assemble(&was);
        let anchor: weacon_codemap::Anchor = "sym:a::go".parse().unwrap();
        let pin = weacon_codemap::Pin {
            fingerprint: index.fingerprint(&anchor).unwrap(),
            anchor,
        };
        fake.docs.lock().unwrap().push(pinned_doc(&pin.to_string()));
        let watched = Watched {
            workspace: workspace("one"),
            main: dir.join("main"),
            worktree: dir.join("main"),
        };
        let moved = |commit: char| Move {
            was: was.clone(),
            now: now.clone(),
            commit: commit.to_string().repeat(40).parse().unwrap(),
        };
        let (log, records) = recording_logger();

        stores.moved(&watched, moved('b'), &log);
        tokio::time::timeout(Duration::from_secs(10), fake.heard.notified())
            .await
            .unwrap();
        stores.moved(&watched, moved('b'), &log);
        stores.moved(&watched, moved('c'), &log);
        tokio::time::timeout(Duration::from_secs(10), fake.heard.notified())
            .await
            .unwrap();

        assert_eq!(fake.commands().len(), 2);
        assert!(!messages(&records).contains(&"anchors check failed"));
        let off = open(&dir, path, None).await.unwrap();
        off.moved(&watched, moved('d'), &log);
        assert!(off.moves.lock().unwrap().is_empty());
    }

    async fn until_failed(stores: &Stores, key: &(WorkspaceId, PathBuf)) {
        let failed = || {
            let checks = stores.moves.lock().unwrap();
            checks
                .get(key)
                .is_some_and(|checked| checked.failed.is_some())
        };
        let waited = async {
            while !failed() {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), waited)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_move_whose_check_failed_is_checked_again_with_the_next() {
        let dir = TempDir::new().unwrap();
        let fake = Fake::default();
        *fake.refuse.lock().unwrap() = Some(code!("proposal.too_many"));
        let path = std::env::var_os("PATH");
        let stores = open(&dir, path, Some(fake.connect())).await.unwrap();
        let manifest = ("Cargo.toml", "[package]\nname = \"a\"\n");
        let was = facts(&[manifest, ("src/lib.rs", "pub fn go() {}\n")]);
        let now = facts(&[manifest, ("src/lib.rs", "pub fn go() { let x = 1; }\n")]);
        let anchor: weacon_codemap::Anchor = "sym:a::go".parse().unwrap();
        let pin = weacon_codemap::Pin {
            fingerprint: assemble(&was).fingerprint(&anchor).unwrap(),
            anchor,
        };
        fake.docs.lock().unwrap().push(pinned_doc(&pin.to_string()));
        let watched = Watched {
            workspace: workspace("one"),
            main: dir.join("main"),
            worktree: dir.join("main"),
        };
        let (log, _) = recording_logger();
        let key = (watched.workspace.clone(), watched.main.clone());

        let first = Move {
            was: was.clone(),
            now: now.clone(),
            commit: "b".repeat(40).parse().unwrap(),
        };
        stores.moved(&watched, first, &log);
        until_failed(&stores, &key).await;
        *fake.refuse.lock().unwrap() = None;
        // The next move changes nothing more, so only the failed one has anything to propose.
        let next = Move {
            was: now.clone(),
            now,
            commit: "c".repeat(40).parse().unwrap(),
        };
        stores.moved(&watched, next, &log);
        tokio::time::timeout(Duration::from_secs(10), fake.heard.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), fake.heard.notified())
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].action, commands[1].action);
    }

    #[tokio::test]
    async fn proposing_maps_hold_where_moves_are_proposed() {
        let fake = Fake::default();

        assert!(Maps::off().proposals.is_none());
        assert!(Maps::off().proposing(fake.connect()).proposals.is_some());
    }

    #[tokio::test]
    async fn a_failed_check_keeps_the_oldest_base_a_later_move_started_after() {
        let fake = Fake::default();
        *fake.refuse.lock().unwrap() = Some(code!("proposal.too_many"));
        let manifest = ("Cargo.toml", "[package]\nname = \"a\"\n");
        let base = |text: &str| facts(&[manifest, ("src/lib.rs", text)]);
        let anchor: weacon_codemap::Anchor = "sym:a::go".parse().unwrap();
        let pin = weacon_codemap::Pin {
            fingerprint: assemble(&base("pub fn go() {}\n"))
                .fingerprint(&anchor)
                .unwrap(),
            anchor,
        };
        fake.docs.lock().unwrap().push(pinned_doc(&pin.to_string()));
        let key = (workspace("one"), PathBuf::from("/main"));
        let commit = |digit: char| -> CommitId { digit.to_string().repeat(40).parse().unwrap() };
        let checks: Arc<Mutex<HashMap<_, Checked>>> = Arc::default();
        let (log, _) = recording_logger();
        // The check of move 1, from base `go`, fails with `failed` kept so far.
        let check = |failed: Option<(u64, Vec<Facts>)>| {
            let checked = Checked {
                commit: commit('d'),
                seq: 3,
                failed,
            };
            checks.lock().unwrap().insert(key.clone(), checked);
            let moved = Move {
                was: base("pub fn go() {}\n"),
                now: base("pub fn go() { let x = 1; }\n"),
                commit: commit('b'),
            };
            let at = (key.clone(), 1);
            check_move(fake.connect(), at, moved, Arc::clone(&checks), log.clone())
        };
        let failed = || checks.lock().unwrap().get(&key).unwrap().failed.clone();
        let (go, older, newer) = (
            base("pub fn go() {}\n"),
            base("pub fn older() {}\n"),
            base("pub fn newer() {}\n"),
        );

        check(None).await;
        assert_eq!(failed(), Some((1, go.clone())));
        // A base kept by an older move's failed check stays.
        check(Some((0, older.clone()))).await;
        assert_eq!(failed(), Some((0, older)));
        // A later move's check that failed first gives way to this older one.
        check(Some((2, newer))).await;
        assert_eq!(failed(), Some((1, go)));
    }
}
