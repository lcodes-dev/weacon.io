//! The code map each run's MCP calls answer from: the core's stored map of the run's
//! repository, only when the run's workspace registered that repository.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use weacon_codemap::Index;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{McpCall, WorkspaceId, code};

use super::tools::{NO_MAP, answer_from};
use crate::app::blocking;
use crate::codemap::{folder_index, main_worktree};
use crate::db::Db;
use crate::git::Git;
use crate::git::repos::Repos;
use crate::workspaces::Workspaces;

/// The longest a call waits for its map; a first read of a large repository can take longer.
const LIMIT: Duration = Duration::from_secs(30);
const TOO_SLOW: &str = "The code map is still being read: try again shortly.\n";

/// The stores the MCP's answers come from.
#[derive(Clone, Debug)]
pub(crate) struct Maps {
    db: Db,
    git: Git,
    workspaces: Workspaces,
    limit: Duration,
}

impl Maps {
    pub(crate) const fn new(db: Db, git: Git, workspaces: Workspaces) -> Self {
        Self {
            db,
            git,
            workspaces,
            limit: LIMIT,
        }
    }

    /// The maps of the database in `state_dir`, with the git on `path`; none, logged, without
    /// git.
    ///
    /// # Errors
    /// When the database can't be opened.
    pub(crate) async fn open(
        state_dir: &Path,
        path: Option<OsString>,
        log: &Logger,
    ) -> Result<Option<Self>, Fault> {
        let db = Db::open(state_dir).await?;
        let Ok(git) = Git::open(state_dir, path.as_deref())
            .inspect_err(|fault| log.warn("mcp maps off", &[("err", Field::error(fault))]))
        else {
            return Ok(None);
        };
        Ok(Some(Self::new(db, git, Workspaces::new(state_dir, path))))
    }

    /// The answer to `call` from the map of `folder`, which `workspace` must have registered
    /// the repository of; "no map" otherwise, or when the map can't be read.
    pub(crate) async fn answer(
        &self,
        call: &McpCall,
        workspace: &WorkspaceId,
        folder: &Path,
        log: &Logger,
    ) -> String {
        let (maps, workspace, folder) = (self.clone(), workspace.clone(), folder.to_owned());
        // Its own task, so a read the limit cuts off still finishes and is stored for the next.
        let reading = tokio::spawn(async move { maps.index(&workspace, folder).await });
        let Ok(joined) = tokio::time::timeout(self.limit, reading).await else {
            return TOO_SLOW.to_owned();
        };
        match joined.map_err(|error| failed().because(error)).flatten() {
            Ok(Some(index)) => answer_from(call, &index),
            Ok(None) => NO_MAP.to_owned(),
            Err(fault) => {
                log.warn("mcp map failed", &[("err", Field::error(&fault))]);
                NO_MAP.to_owned()
            }
        }
    }

    /// The index of `folder`, or none when it isn't in a repository `workspace` registered.
    async fn index(
        &self,
        workspace: &WorkspaceId,
        folder: PathBuf,
    ) -> Result<Option<Index>, Fault> {
        let repos = Repos::new(
            workspace.clone(),
            self.workspaces.root(workspace).join("git/repos.json"),
        );
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

    use weacon_protocol::{MapArgs, RepoId};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::db::{BASE, LocalRepo};
    use crate::git::{fixture, git, repo};

    pub(crate) struct Setup {
        pub(crate) maps: Maps,
        pub(crate) workspace: WorkspaceId,
        list: PathBuf,
    }

    /// Maps over a new database in `dir`, and the repositories of workspace `one`.
    pub(crate) async fn setup(dir: &Path) -> Setup {
        let state = dir.join("state");
        fs::create_dir(&state).unwrap();
        let db = Db::open(&state).await.unwrap();
        let workspaces = Workspaces::new(&state, None);
        let workspace = WorkspaceId::try_from("one".to_owned()).unwrap();
        let list = workspaces.root(&workspace).join("git/repos.json");
        Setup {
            maps: Maps::new(db, git(dir), workspaces),
            workspace,
            list,
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

    /// Registers `repo` for `workspace` as `id`.
    pub(crate) fn register(maps: &Maps, workspace: &WorkspaceId, id: &str, repo: &Path) {
        let list = maps.workspaces.root(workspace).join("git/repos.json");
        let id = RepoId::try_from(id.to_owned()).unwrap();
        Repos::new(workspace.clone(), list)
            .add(&maps.git, id, repo)
            .unwrap();
    }

    fn map() -> McpCall {
        McpCall::Map(MapArgs::default())
    }

    #[tokio::test]
    async fn a_registered_repository_answers_from_its_stored_map() {
        let dir = TempDir::new().unwrap();
        let Setup {
            maps, workspace, ..
        } = setup(&dir).await;
        let main = crate_repo(&dir, "marker");
        register(&maps, &workspace, "site", &main);
        let (log, records) = recording_logger();

        let answer = maps
            .answer(&map(), &workspace, &main.join("src"), &log)
            .await;

        assert!(answer.contains("fn marker::run"), "{answer}");
        let stored = maps.db.workspace(&workspace).map(&LocalRepo::at(&main));
        assert!(stored.layer(BASE).await.unwrap().is_some());
        assert!(records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_repository_the_workspace_never_registered_has_no_map_and_stores_none() {
        let dir = TempDir::new().unwrap();
        let Setup {
            maps, workspace, ..
        } = setup(&dir).await;
        let main = crate_repo(&dir, "one");
        let other = crate_repo(&dir.join("other"), "two");
        register(&maps, &workspace, "site", &other);
        let (log, records) = recording_logger();

        let unregistered = maps.answer(&map(), &workspace, &main, &log).await;
        let no_repo = maps
            .answer(&map(), &workspace, &dir.join("state"), &log)
            .await;

        assert_eq!((unregistered.as_str(), no_repo.as_str()), (NO_MAP, NO_MAP));
        let stored = maps.db.workspace(&workspace).map(&LocalRepo::at(&main));
        assert_eq!(stored.layer(BASE).await.unwrap(), None);
        assert!(records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_map_slower_than_the_limit_says_so_and_a_broken_list_is_logged() {
        let dir = TempDir::new().unwrap();
        let Setup {
            mut maps,
            workspace,
            list,
        } = setup(&dir).await;
        let main = crate_repo(&dir, "one");
        let (log, records) = recording_logger();
        maps.limit = Duration::ZERO;

        let late = maps.answer(&map(), &workspace, &main, &log).await;
        maps.limit = LIMIT;
        fs::create_dir_all(&list).unwrap();
        let broken = maps.answer(&map(), &workspace, &main, &log).await;

        assert_eq!((late.as_str(), broken.as_str()), (TOO_SLOW, NO_MAP));
        assert_eq!(weacon_testing::messages(&records), ["mcp map failed"]);
    }

    #[tokio::test]
    async fn the_daemon_gets_maps_only_with_git_and_fails_without_a_database() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        fs::create_dir(&state).unwrap();
        let (log, records) = recording_logger();

        let found = Maps::open(&state, std::env::var_os("PATH"), &log).await;
        let no_git = Maps::open(&state, Some(dir.join("empty").into()), &log).await;
        let no_db = Maps::open(&dir.join("missing"), None, &log).await;

        assert!(found.unwrap().is_some());
        assert!(no_git.unwrap().is_none());
        assert_eq!(no_db.unwrap_err().code().as_str(), "db.open_failed");
        assert_eq!(weacon_testing::messages(&records), ["mcp maps off"]);
    }
}
