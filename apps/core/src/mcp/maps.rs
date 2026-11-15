//! The code map each run's MCP calls answer from: the core's stored map of the run's
//! repository, only when the run's workspace registered that repository, kept current by a
//! watch of each of its worktrees.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{OnceCell, OwnedSemaphorePermit};
use tokio::time::MissedTickBehavior;
use weacon_codemap::{Index, assemble};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{McpCall, WorkspaceId, code};

use super::grants::Grant;
use super::tools::{NO_MAP, answer_from};
use crate::app::{WarnOnce, blocking};
use crate::chats::Places;
use crate::codemap::{Repo, Watch, locate, off_runtime, watch, worktree_index};
use crate::db::Db;
use crate::git::Git;
use crate::git::repos::Repos;
use crate::workspaces::Workspaces;

/// The longest a call waits for its map; a first read of a large repository can take longer.
const LIMIT: Duration = Duration::from_secs(30);
const TOO_SLOW: &str = "The code map is still being read: try again shortly.\n";
/// How often each watched worktree is checked for changes.
const POLL: Duration = Duration::from_secs(5);

/// The stores the MCP's answers come from, opened at the first call, once the daemon holds
/// its state folder, so a second daemon that can't start changes nothing.
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
    /// The current index of each watched worktree, by its workspace and path.
    live: Arc<Mutex<HashMap<Watched, Arc<Index>>>>,
}

/// A worktree of a repository a workspace registered, by its canonical paths.
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
        let (workspace, folder, log) = (grant.workspace.clone(), grant.folder.clone(), log.clone());
        // Its own task, so a read the limit cuts off still finishes and is stored for the next.
        let reading = tokio::spawn(async move {
            let _permit = permit;
            let index = stores.index(workspace, folder).await;
            index
                .inspect_err(|fault| log.warn("mcp map failed", &[("err", Field::error(fault))]))
                .ok()
                .flatten()
        });
        match tokio::time::timeout(self.limit, reading).await {
            Err(_) => TOO_SLOW.to_owned(),
            Ok(Ok(Some(index))) => answer_from(call, &index),
            Ok(_) => NO_MAP.to_owned(),
        }
    }

    /// Keeps the index of every worktree of each registered repository current, checking them
    /// every [`POLL`]; it runs until dropped.
    #[expect(
        clippy::infinite_loop,
        reason = "it runs until the server's task is dropped"
    )]
    pub(crate) async fn keep_current(&self, log: &Logger) {
        let Some(stores) = self.stores(log).await else {
            return;
        };
        let (mut watches, mut listing) = (HashMap::new(), WarnOnce::default());
        let mut poll = tokio::time::interval(POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            poll.tick().await;
            let found = stores.watched().await;
            if let Some(found) = listing.check(found, log, "map list failed") {
                stores.check(&mut watches, found, log).await;
            }
        }
    }

    /// The stores, opened at the first call; none, logged once, when either can't be.
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
    let workspaces = Workspaces::new(state_dir, path);
    let live = Arc::default();
    Ok(Stores {
        db,
        git,
        workspaces,
        live,
    })
}

impl Stores {
    /// The index of `folder`, or none when it isn't in a repository `workspace` registered:
    /// its watch's when it has one, else read now.
    async fn index(
        &self,
        workspace: WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<Arc<Index>>, Fault> {
        let (stores, asked) = (self.clone(), workspace.clone());
        let located = blocking(move || Ok(stores.registered(&asked, &folder)))
            .await
            .map_err(|error| failed().because(error))??;
        let Some((main, worktree)) = located else {
            return Ok(None);
        };
        let watched = Watched {
            workspace,
            main,
            worktree,
        };
        if let Some(index) = self.live().get(&watched) {
            return Ok(Some(Arc::clone(index)));
        }
        let Watched {
            workspace,
            main,
            worktree,
        } = watched;
        let store = self.db.workspace(&workspace);
        let index = worktree_index(&store, &self.git, main, &worktree).await?;
        Ok(Some(Arc::new(index)))
    }

    /// The main worktree and the worktree of `folder`'s repository, when it is one `workspace`
    /// registered; a folder in no repository isn't. It blocks.
    fn registered(
        &self,
        workspace: &WorkspaceId,
        folder: &Path,
    ) -> Result<Option<(PathBuf, PathBuf)>, Fault> {
        let Ok((main, worktree)) = locate(&self.git, folder) else {
            return Ok(None);
        };
        let repos = Repos::of(&self.workspaces, workspace.clone()).list()?;
        Ok(repos
            .iter()
            .any(|repo| repo.path == main)
            .then_some((main, worktree)))
    }

    /// Every worktree of each repository each workspace registered; a repository git can't
    /// list is left out.
    async fn watched(&self) -> Result<Vec<Watched>, Fault> {
        let stores = self.clone();
        blocking(move || Ok(stores.list()))
            .await
            .map_err(|error| failed().because(error))?
    }

    fn list(&self) -> Result<Vec<Watched>, Fault> {
        let known = self
            .workspaces
            .known()
            .map_err(|error| failed().because(error))?;
        let mut found = Vec::new();
        for workspace in known {
            for repo in Repos::of(&self.workspaces, workspace.clone()).list()? {
                found.extend(self.worktrees(&workspace, &repo.path));
            }
        }
        Ok(found)
    }

    /// The worktrees of the repository at `repo`, the main one first; none when git can't
    /// list them.
    fn worktrees(&self, workspace: &WorkspaceId, repo: &Path) -> Vec<Watched> {
        let trees: Vec<PathBuf> = self
            .git
            .worktrees(repo)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tree| tree.path.canonicalize().ok())
            .collect();
        let Some(main) = trees.first().cloned() else {
            return Vec::new();
        };
        trees
            .into_iter()
            .map(|worktree| Watched {
                workspace: workspace.clone(),
                main: main.clone(),
                worktree,
            })
            .collect()
    }

    /// Checks each of `found`, keeping a watch for each, and forgets the watches and indexes of
    /// worktrees no longer found.
    async fn check(
        &self,
        watches: &mut HashMap<Watched, Watch<Repo>>,
        found: Vec<Watched>,
        log: &Logger,
    ) {
        watches.retain(|watched, _| found.contains(watched));
        self.live().retain(|watched, _| found.contains(watched));
        for watched in found {
            let workspace = self.db.workspace(&watched.workspace);
            let watch = watches.entry(watched.clone()).or_insert_with(|| {
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

fn failed() -> Fault {
    Fault::new(code!("mcp.map_failed"), "The code map couldn't be read")
}

#[cfg(test)]
pub(super) mod tests {
    use std::fs;

    use tokio::sync::Semaphore;
    use weacon_protocol::{MapArgs, RepoId};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::db::{BASE, LocalRepo};
    use crate::git::{fixture, git, repo};

    pub(crate) struct Setup {
        pub(crate) maps: Maps,
        pub(crate) db: Db,
        git: Git,
        workspaces: Workspaces,
    }

    /// Maps over a new state folder in `dir`, and its own handles on the same stores.
    pub(crate) async fn setup(dir: &Path) -> Setup {
        let state = dir.join("state");
        fs::create_dir(&state).unwrap();
        let places = Places {
            path: std::env::var_os("PATH"),
            ..Places::default()
        };
        Setup {
            maps: Maps::new(&state, &places),
            db: Db::open(&state).await.unwrap(),
            git: git(dir),
            workspaces: Workspaces::new(&state, None),
        }
    }

    impl Setup {
        /// Registers `repo` for `workspace` as `id`.
        pub(crate) fn register(&self, workspace: &WorkspaceId, id: &str, repo: &Path) {
            let id = RepoId::try_from(id.to_owned()).unwrap();
            let repos = Repos::of(&self.workspaces, workspace.clone());
            repos.add(&self.git, id, repo).unwrap();
        }

        /// Whether `workspace` holds a stored base for `repo`.
        pub(crate) async fn stored(&self, workspace: &WorkspaceId, repo: &Path) -> bool {
            let map = self.db.workspace(workspace).map(&LocalRepo::at(repo));
            map.layer(BASE).await.unwrap().is_some()
        }
    }

    /// A repository at `dir/repo` holding the crate `name`, with its `run`, committed.
    pub(crate) fn crate_repo(dir: &Path, name: &str) -> PathBuf {
        let main = repo(dir);
        fs::create_dir_all(main.join("src")).unwrap();
        let manifest = format!("[package]\nname = \"{name}\"\n");
        fs::write(main.join("Cargo.toml"), manifest).unwrap();
        fs::write(main.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
        fixture(dir, &main, &["add", "Cargo.toml", "src"]);
        fixture(dir, &main, &["commit", "--quiet", "-m", "Add a crate"]);
        main.canonicalize().unwrap()
    }

    pub(crate) fn workspace(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

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
        assert_eq!(weacon_testing::messages(&records), ["mcp map failed"]);
    }

    #[tokio::test]
    async fn watches_keep_each_registered_worktrees_index_until_it_is_unregistered() {
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

        let found = stores.watched().await.unwrap();
        assert_eq!(
            found
                .iter()
                .map(|watched| &watched.worktree)
                .collect::<Vec<_>>(),
            [&main]
        );
        stores.check(&mut watches, found, &log).await;
        fs::write(main.join("src/lib.rs"), "pub fn walk() {}\n").unwrap();
        // Served from the watch, which hasn't looked again yet.
        assert!(ask().await.contains("fn one::run"));
        stores
            .check(&mut watches, stores.watched().await.unwrap(), &log)
            .await;
        assert!(ask().await.contains("fn one::walk"));

        fs::remove_file(setup.workspaces.root(&one).join("git/repos.json")).unwrap();
        stores
            .check(&mut watches, stores.watched().await.unwrap(), &log)
            .await;
        assert!(watches.is_empty() && stores.live().is_empty());
        assert_eq!(ask().await, NO_MAP);
    }

    async fn until_watched(stores: &Stores) {
        while stores.live().is_empty() {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn keeping_current_watches_from_its_first_check() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let one = workspace("one");
        let main = crate_repo(&dir, "one");
        setup.register(&one, "site", &main);
        let (log, _records) = recording_logger();
        let stores = setup.maps.stores(&log).await.unwrap();
        let maps = setup.maps.clone();
        let keeping = tokio::spawn(async move { maps.keep_current(&log).await });

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
