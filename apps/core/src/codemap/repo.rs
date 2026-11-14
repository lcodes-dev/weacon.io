//! The code map's source for a repository on this machine, read through git.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use weacon_codemap::Index;
use weacon_observability::Fault;
use weacon_protocol::{CommitId, code};

use super::{RepoMap, Source};
use crate::app::blocking;
use crate::db::{LocalRepo, Workspace};
use crate::git::Git;

/// A repository on disk, its main worktree at `repo`.
#[derive(Clone, Debug)]
pub(crate) struct Repo {
    pub(crate) git: Git,
    pub(crate) repo: PathBuf,
}

impl Source for Repo {
    fn head(&self) -> Result<CommitId, Fault> {
        self.git.head(&self.repo)
    }

    fn tree(&self, commit: &CommitId) -> Result<Vec<(String, String)>, Fault> {
        self.git.tree(&self.repo, commit)
    }

    fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault> {
        self.git.blobs(&self.repo, blobs)
    }

    fn changed(
        &self,
        path: &Path,
        commit: &CommitId,
    ) -> Result<Vec<(String, Option<String>)>, Fault> {
        let changed = self.git.changed(path, commit)?;
        Ok(changed
            .into_iter()
            .map(|file| {
                let text = text_at(&path.join(&file));
                (file, text)
            })
            .collect())
    }
}

/// The index of the repository `folder` is in, as the worktree holding `folder` has it, kept in
/// `workspace`'s map so that only text it hasn't seen is read.
pub(crate) async fn folder_index(
    workspace: &Workspace,
    git: &Git,
    folder: &Path,
) -> Result<Index, Fault> {
    let (main, worktree) = {
        let (git, folder) = (git.clone(), folder.to_owned());
        blocking(move || Ok(locate(&git, &folder)))
            .await
            .map_err(|error| not_a_repo().because(error))??
    };
    let repo = RepoMap {
        map: workspace.map(&LocalRepo::at(&main)),
        source: Repo {
            git: git.clone(),
            repo: main,
        },
    };
    let base = repo.base().await?;
    let changed = repo.changed(&base, &worktree).await?;
    let facts = repo.worktree(&base, &worktree, changed).await?;
    Ok(weacon_codemap::assemble(&facts))
}

/// The canonical paths of the main worktree of `folder`'s repository and of the worktree
/// `folder` is in.
fn locate(git: &Git, folder: &Path) -> Result<(PathBuf, PathBuf), Fault> {
    let folder = folder
        .canonicalize()
        .map_err(|error| not_a_repo().because(error))?;
    let trees: Vec<PathBuf> = git
        .worktrees(&folder)?
        .into_iter()
        .filter_map(|tree| tree.path.canonicalize().ok())
        .collect();
    let main = trees.first().cloned().ok_or_else(not_a_repo)?;
    let worktree = trees
        .into_iter()
        .filter(|tree| folder.starts_with(tree))
        .max_by_key(|tree| tree.components().count())
        .ok_or_else(not_a_repo)?;
    Ok((main, worktree))
}

fn not_a_repo() -> Fault {
    Fault::new(
        code!("map.not_a_repo"),
        "The folder isn't in a git repository",
    )
}

/// The text of the regular file at `path`; a link, a folder, a missing file and one that isn't
/// UTF-8 have none.
fn text_at(path: &Path) -> Option<String> {
    let kind = std::fs::symlink_metadata(path).ok()?;
    kind.is_file().then(|| std::fs::read_to_string(path).ok())?
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::WorkspaceId;
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;
    use crate::git::tests::{fixture, git, repo};

    #[test]
    fn reads_a_repository_and_what_its_worktree_changed() {
        let dir = TempDir::new().unwrap();
        let path = repo(&dir);
        let source = Repo {
            git: git(&dir),
            repo: path.clone(),
        };
        let head = source.head().unwrap();
        let tree = source.tree(&head).unwrap();
        let blobs: Vec<String> = tree.iter().map(|(_, blob)| blob.clone()).collect();
        let texts = source.blobs(&blobs).unwrap();
        assert!(texts.values().any(|text| text == "# Site\n"));

        fs::write(path.join("README.md"), "# Changed\n").unwrap();
        fs::write(path.join("bytes.md"), [0xff]).unwrap();
        fs::remove_file(path.join(".gitattributes")).unwrap();
        fs::create_dir(path.join("linked")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", path.join("link.md")).unwrap();
        fixture(&dir, &path, &["add", "--all"]);

        let changed = source.changed(&path, &head).unwrap();
        let mut expected = vec![
            (".gitattributes".to_owned(), None),
            ("README.md".to_owned(), Some("# Changed\n".to_owned())),
            ("bytes.md".to_owned(), None),
        ];
        if cfg!(unix) {
            expected.push(("link.md".to_owned(), None));
        }
        assert_eq!(changed, expected);
    }

    #[tokio::test]
    async fn indexes_a_folder_as_its_worktree_has_it() {
        let dir = TempDir::new().unwrap();
        let main = repo(&dir);
        fs::create_dir_all(main.join("a/src")).unwrap();
        fs::write(main.join("a/Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        fs::write(main.join("a/src/lib.rs"), "pub fn run() {}\n").unwrap();
        fixture(&dir, &main, &["add", "a"]);
        fixture(&dir, &main, &["commit", "--quiet", "-m", "Add a"]);
        let other = dir.join("other");
        let other_arg = other.to_str().unwrap();
        fixture(
            &dir,
            &main,
            &["worktree", "add", "--quiet", "-b", "other", other_arg],
        );
        fs::write(other.join("a/src/lib.rs"), "pub fn walk() {}\n").unwrap();
        fs::create_dir(dir.join("state")).unwrap();
        let db = Db::open(&dir.join("state")).await.unwrap();
        let workspace = db.workspace(&WorkspaceId::try_from("one".to_owned()).unwrap());
        let git = git(&dir);

        let symbol = |index: &Index, path: &str| index.symbol(path).is_some();
        let in_main = folder_index(&workspace, &git, &main).await.unwrap();
        assert!(symbol(&in_main, "a::run"));
        let in_other = folder_index(&workspace, &git, &other.join("a/src"))
            .await
            .unwrap();
        assert!(symbol(&in_other, "a::walk"));
        assert!(!symbol(&in_other, "a::run"));
        let main_key = LocalRepo::at(&main.canonicalize().unwrap());
        assert!(
            workspace
                .map(&main_key)
                .layer(crate::db::BASE)
                .await
                .unwrap()
                .is_some()
        );

        let fault = folder_index(&workspace, &git, &dir.join("state"))
            .await
            .unwrap_err();
        assert_eq!(fault.code().as_str(), "git.failed");
        let fault = folder_index(&workspace, &git, &dir.join("missing"))
            .await
            .unwrap_err();
        assert_eq!(fault.code().as_str(), "map.not_a_repo");
    }
}
