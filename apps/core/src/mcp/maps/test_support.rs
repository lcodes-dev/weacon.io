//! What the MCP's tests build their stored maps from.

use std::fs;
use std::path::{Path, PathBuf};

use weacon_protocol::{RepoId, WorkspaceId};

use super::Maps;
use crate::chats::Places;
use crate::db::{BASE, Db, LocalRepo};
use crate::git::repos::Repos;
use crate::git::{Git, fixture, git, repo};
use crate::workspaces::Workspaces;

pub(crate) struct Setup {
    pub(crate) maps: Maps,
    pub(crate) db: Db,
    pub(crate) git: Git,
    pub(crate) workspaces: Workspaces,
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
