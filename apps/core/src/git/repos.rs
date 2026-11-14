//! The repositories a workspace's chats may work in on this machine, which the owner adds.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{RepoId, WorkspaceId, code};

use super::{Git, state};
use crate::app::{App, state_unavailable};
use crate::cli::RepoCommand;
use crate::workspaces::Workspaces;

/// A repository's id in its workspace, and its top folder here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Repo {
    pub(crate) workspace: WorkspaceId,
    pub(crate) id: RepoId,
    pub(crate) path: PathBuf,
}

/// One workspace's repositories, a JSON file written whole. Every call blocks.
#[derive(Debug)]
pub(crate) struct Repos {
    workspace: WorkspaceId,
    file: PathBuf,
}

/// Adds a repository to a workspace, or lists them, writing the list to `out`.
///
/// # Errors
/// When the state folder or git is missing, or adding fails.
pub(crate) fn run(app: &App, command: RepoCommand, out: &mut impl Write) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let workspaces = Workspaces::new(state_dir, app.path.clone());
    let repos = |workspace: WorkspaceId| {
        let file = workspaces.root(&workspace).join("git/repos.json");
        Repos::new(workspace, file)
    };
    match command {
        RepoCommand::Add {
            workspace,
            id,
            path,
        } => {
            let git = Git::open(state_dir, app.path.as_deref())?;
            let repo = repos(workspace).add(&git, id, &path)?;
            let ids = [("workspace_id", repo.workspace.as_str().into())];
            app.log.info("repo added", &ids);
            Ok(())
        }
        RepoCommand::List { workspace } => {
            for repo in repos(workspace).list()? {
                writeln!(out, "{}\t{}", repo.id.as_str(), repo.path.display())
                    .map_err(|error| printing().because(error))?;
            }
            Ok(())
        }
    }
}

impl Repos {
    pub(crate) const fn new(workspace: WorkspaceId, file: PathBuf) -> Self {
        Self { workspace, file }
    }

    /// Adds the repository whose top folder is `path` as `id`. Adding the same again changes
    /// nothing; an id or folder already taken by another is refused.
    pub(crate) fn add(&self, git: &Git, id: RepoId, path: &Path) -> Result<Repo, Fault> {
        if !path.is_absolute() {
            return Err(Fault::new(
                code!("repo.path_relative"),
                "A repository's folder needs an absolute path",
            ));
        }
        let folder = path.canonicalize().map_err(|error| {
            Fault::new(code!("repo.not_found"), "That folder isn't there").because(error)
        })?;
        if git.top_level(&folder)? != folder {
            return Err(Fault::new(
                code!("repo.not_top"),
                "That folder isn't the top of a git repository",
            ));
        }
        let mut repos = state::load::<Repo>(&self.file).map_err(|error| failed().because(error))?;
        let ours = |repo: &&Repo| repo.workspace == self.workspace;
        if let Some(kept) = repos
            .iter()
            .filter(ours)
            .find(|repo| repo.id == id || repo.path == folder)
        {
            return if kept.id == id && kept.path == folder {
                Ok(kept.clone())
            } else {
                Err(Fault::new(
                    code!("repo.taken"),
                    "That id or folder is already another repository's",
                ))
            };
        }
        let repo = Repo {
            workspace: self.workspace.clone(),
            id,
            path: folder,
        };
        repos.push(repo.clone());
        state::save(&self.file, &repos).map_err(|error| failed().because(error))?;
        Ok(repo)
    }

    /// The repository `id`, if this workspace has it.
    pub(crate) fn get(&self, id: &RepoId) -> Result<Option<Repo>, Fault> {
        Ok(self.list()?.into_iter().find(|repo| repo.id == *id))
    }

    /// This workspace's repositories, in the order they were added.
    pub(crate) fn list(&self) -> Result<Vec<Repo>, Fault> {
        let repos = state::load::<Repo>(&self.file).map_err(|error| failed().because(error))?;
        Ok(repos
            .into_iter()
            .filter(|repo| repo.workspace == self.workspace)
            .collect())
    }
}

fn failed() -> Fault {
    Fault::new(
        code!("repo.list_failed"),
        "The workspace's repositories couldn't be read or written",
    )
}

fn printing() -> Fault {
    Fault::new(
        code!("repo.print_failed"),
        "The repositories couldn't be printed",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{git, repo};
    use crate::test_support::app;

    fn repos(dir: &Path, workspace: &str) -> Repos {
        let id = WorkspaceId::try_from(workspace.to_owned()).unwrap();
        Repos::new(id, dir.join("repos.json"))
    }

    fn id(id: &str) -> RepoId {
        RepoId::try_from(id.to_owned()).unwrap()
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    #[test]
    fn a_repository_is_added_by_its_top_folder_and_found_by_id() {
        let dir = TempDir::new().unwrap();
        let (git, folder) = (git(&dir), repo(&dir));
        let ours = repos(&dir, "w-1");

        let added = ours.add(&git, id("site"), &folder).unwrap();
        let again = ours.add(&git, id("site"), &folder).unwrap();

        assert_eq!(added.path, folder.canonicalize().unwrap());
        assert_eq!(again, added);
        assert_eq!(ours.get(&id("site")).unwrap(), Some(added.clone()));
        assert_eq!(ours.get(&id("other")).unwrap(), None);
        assert_eq!(ours.list().unwrap(), [added]);
    }

    #[test]
    fn only_a_repositorys_top_folder_given_in_full_is_added() {
        let dir = TempDir::new().unwrap();
        let (git, folder) = (git(&dir), repo(&dir));
        fs::create_dir(folder.join("src")).unwrap();
        fs::create_dir(dir.join("plain")).unwrap();
        let ours = repos(&dir, "w-1");
        let refused = |path: &Path| code(&ours.add(&git, id("site"), path).unwrap_err()).to_owned();

        assert_eq!(refused(Path::new("repo")), "repo.path_relative");
        assert_eq!(refused(&dir.join("missing")), "repo.not_found");
        assert_eq!(refused(&folder.join("src")), "repo.not_top");
        assert_eq!(refused(&dir.join("plain")), "git.failed");
        assert_eq!(ours.list().unwrap(), []);
    }

    #[test]
    fn an_id_or_folder_is_one_repositorys_in_a_workspace() {
        let dir = TempDir::new().unwrap();
        let (git, folder) = (git(&dir), repo(&dir));
        let ours = repos(&dir, "w-1");
        ours.add(&git, id("site"), &folder).unwrap();

        let error = ours.add(&git, id("again"), &folder).unwrap_err();
        assert_eq!(code(&error), "repo.taken");
        let other = dir.join("other");
        fs::create_dir(&other).unwrap();
        crate::git::tests::fixture(&dir, &other, &["init", "--quiet"]);
        let error = ours.add(&git, id("site"), &other).unwrap_err();
        assert_eq!(code(&error), "repo.taken");

        let theirs = repos(&dir, "w-2");
        assert_eq!(theirs.get(&id("site")).unwrap(), None);
        theirs.add(&git, id("site"), &other).unwrap();
        assert_eq!(
            ours.get(&id("site")).unwrap().unwrap().path,
            folder.canonicalize().unwrap()
        );
    }

    #[test]
    fn a_list_that_cant_be_read_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.join("repos.json")).unwrap();

        assert_eq!(
            code(&repos(&dir, "w-1").list().unwrap_err()),
            "repo.list_failed"
        );
    }

    #[test]
    fn the_command_adds_and_lists_a_workspaces_repositories() {
        let dir = TempDir::new().unwrap();
        let folder = repo(&dir);
        let state = dir.join("state");
        let mut app = app(Some(&state), None);
        app.path = std::env::var_os("PATH");
        let workspace = WorkspaceId::try_from("w-1".to_owned()).unwrap();
        let add = RepoCommand::Add {
            workspace: workspace.clone(),
            id: id("site"),
            path: folder.clone(),
        };
        let mut out = Vec::new();

        run(&app, add, &mut out).unwrap();
        run(&app, RepoCommand::List { workspace }, &mut out).unwrap();

        let real = folder.canonicalize().unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("site\t{}\n", real.display())
        );
        let without = crate::test_support::app(None, None);
        let list = RepoCommand::List {
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
        };
        assert_eq!(
            code(&run(&without, list, &mut Vec::new()).unwrap_err()),
            "state.unavailable"
        );
    }
}
