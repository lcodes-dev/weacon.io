//! The code map's source for a repository on this machine, read through git.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use weacon_codemap::{Churn, Index};
use weacon_observability::Fault;
use weacon_protocol::{CommitId, code};

use super::watch::Watch;
use super::{RepoMap, Source, off_runtime};
use crate::db::{LocalRepo, Workspace};
use crate::git::Git;
use crate::platform;

/// The largest file the map reads; a larger one is left out, likely generated or not code.
pub(crate) const LARGEST: u64 = 1 << 20;
/// How many of the latest commits hotspots count changes in.
const CHURN_COMMITS: u32 = 500;

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
        let tree = self.git.tree(&self.repo, commit)?;
        Ok(tree
            .into_iter()
            .filter(|file| file.size <= LARGEST)
            .map(|file| (file.path, file.blob))
            .collect())
    }

    fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault> {
        self.git.blobs(&self.repo, blobs)
    }

    fn changed(&self, path: &Path, commit: &CommitId) -> Result<Vec<String>, Fault> {
        self.git.changed(path, commit)
    }

    /// Only a regular file of at most [`LARGEST`] bytes reached through no link, inside the
    /// worktree, has text.
    fn text(&self, path: &Path, file: &str) -> Option<String> {
        let file = platform::open_beneath(&path.canonicalize().ok()?, file).ok()?;
        let about = file.metadata().ok()?;
        if !about.is_file() || about.len() > LARGEST {
            return None;
        }
        let mut text = String::new();
        file.take(LARGEST).read_to_string(&mut text).ok()?;
        Some(text)
    }
}

/// The index of the repository whose main worktree is `main`, as its worktree `worktree` has
/// it, kept in `workspace`'s map so that only text it hasn't seen is read.
pub(crate) async fn worktree_index(
    workspace: &Workspace,
    git: &Git,
    main: PathBuf,
    worktree: &Path,
) -> Result<Index, Fault> {
    let repo = repo_map(workspace, git, main);
    let base = repo.base().await?;
    let changed = repo.changed(&base, worktree).await?;
    let facts = repo.worktree(&base, worktree, changed).await?;
    off_runtime(move || Ok(weacon_codemap::assemble(&facts))).await
}

/// A watch of `worktree`, in the repository whose main worktree is `main`, over `workspace`'s
/// map.
pub(crate) fn watch(
    workspace: &Workspace,
    git: &Git,
    main: PathBuf,
    worktree: PathBuf,
) -> Watch<Repo> {
    Watch::new(repo_map(workspace, git, main), worktree)
}

fn repo_map(workspace: &Workspace, git: &Git, main: PathBuf) -> RepoMap<Repo> {
    RepoMap {
        map: workspace.map(&LocalRepo::at(&main)),
        source: Repo {
            git: git.clone(),
            repo: main,
        },
    }
}

/// How many of the latest [`CHURN_COMMITS`] commits up to its head changed each file under
/// `folder`, by its path from `folder`; it blocks.
pub(crate) fn churn(git: &Git, folder: &Path) -> Result<Vec<Churn>, Fault> {
    let head = git.head(folder)?;
    let churn = git.churn(folder, &head, CHURN_COMMITS)?;
    Ok(churn
        .into_iter()
        .map(|(file, commits)| Churn { file, commits })
        .collect())
}

/// The canonical paths of the main worktree of `folder`'s repository and of the worktree
/// `folder` is in; it blocks.
///
/// # Errors
/// When `folder` isn't in a git repository.
pub(crate) fn locate(git: &Git, folder: &Path) -> Result<(PathBuf, PathBuf), Fault> {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::WorkspaceId;
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Db;
    use crate::git::{fixture, git, repo};

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
        let largest = usize::try_from(LARGEST).unwrap();
        fs::write(path.join("big.md"), "x".repeat(largest + 1)).unwrap();
        fs::write(path.join("full.md"), "x".repeat(largest)).unwrap();
        fs::remove_file(path.join(".gitattributes")).unwrap();
        let text = |file: &str| source.text(&path, file);
        assert_eq!(text("README.md").as_deref(), Some("# Changed\n"));
        assert_eq!(text("bytes.md"), None);
        assert_eq!(text("big.md"), None);
        assert_eq!(text("full.md").map(|text| text.len()), Some(largest));
        assert_eq!(text(".gitattributes"), None);
        let changed = source.changed(&path, &head).unwrap();
        assert_eq!(
            changed,
            [
                ".gitattributes",
                "README.md",
                "big.md",
                "bytes.md",
                "full.md"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn reads_nothing_through_a_link() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let path = repo(&dir);
        let source = Repo {
            git: git(&dir),
            repo: path.clone(),
        };
        let outside = dir.join("outside");
        fs::create_dir_all(outside.join("src")).unwrap();
        fs::write(outside.join("secret.md"), "outside").unwrap();
        fs::write(outside.join("src/lib.rs"), "outside").unwrap();
        fs::write(path.join("inside.md"), "inside").unwrap();
        symlink(outside.join("secret.md"), path.join("link.md")).unwrap();
        symlink(&outside, path.join("docs")).unwrap();
        symlink(path.join("inside.md"), path.join("near.md")).unwrap();

        assert_eq!(source.text(&path, "inside.md").as_deref(), Some("inside"));
        for file in [
            "link.md",
            "docs/secret.md",
            "docs/src/lib.rs",
            "near.md",
            "docs",
            "../outside/secret.md",
        ] {
            assert_eq!(source.text(&path, file), None, "{file}");
        }
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
        let index = async |folder: &Path| {
            let (main, worktree) = locate(&git, folder).unwrap();
            worktree_index(&workspace, &git, main, &worktree)
                .await
                .unwrap()
        };
        assert!(symbol(&index(&main).await, "a::run"));
        let in_other = index(&other.join("a/src")).await;
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

        let fault = locate(&git, &dir.join("state")).unwrap_err();
        assert_eq!(fault.code().as_str(), "git.failed");
        let fault = locate(&git, &dir.join("missing")).unwrap_err();
        assert_eq!(fault.code().as_str(), "map.not_a_repo");
    }
}
