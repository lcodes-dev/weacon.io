//! The code map each run's MCP calls answer from: the core's stored map of the run's
//! repository, only when the run's workspace registered that repository.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OnceCell, OwnedSemaphorePermit};
use weacon_codemap::Index;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{McpCall, WorkspaceId, code};

use super::grants::Grant;
use super::tools::{NO_MAP, answer_from};
use crate::app::blocking;
use crate::chats::Places;
use crate::codemap::{folder_index, main_worktree};
use crate::db::Db;
use crate::git::Git;
use crate::git::repos::Repos;
use crate::workspaces::Workspaces;

/// The longest a call waits for its map; a first read of a large repository can take longer.
const LIMIT: Duration = Duration::from_secs(30);
const TOO_SLOW: &str = "The code map is still being read: try again shortly.\n";

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
            let index = stores.index(&workspace, folder).await;
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
    Ok(Stores {
        db,
        git,
        workspaces,
    })
}

impl Stores {
    /// The index of `folder`, or none when it isn't in a repository `workspace` registered.
    async fn index(
        &self,
        workspace: &WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<Index>, Fault> {
        let repos = Repos::of(&self.workspaces, workspace.clone());
        let (git, asked) = (self.git.clone(), folder.clone());
        let registered = blocking(move || Ok(registered(&git, &repos, &asked)))
            .await
            .map_err(|error| failed().because(error))??;
        if !registered {
            return Ok(None);
        }
        let store = self.db.workspace(workspace);
        folder_index(&store, &self.git, &folder).await.map(Some)
    }
}

/// Whether `folder`'s repository is one of `repos`; a folder in no repository isn't.
fn registered(git: &Git, repos: &Repos, folder: &Path) -> Result<bool, Fault> {
    let Ok(main) = main_worktree(git, folder) else {
        return Ok(false);
    };
    Ok(repos.list()?.iter().any(|repo| repo.path == main))
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
