//! The code map's source for a repository on this machine, read through git.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use weacon_observability::Fault;
use weacon_protocol::{CommitId, code};

use super::Source;
use crate::git::Git;

/// A repository on disk, its main worktree at `repo`.
#[derive(Clone, Debug)]
pub(crate) struct Repo {
    pub(crate) git: Git,
    pub(crate) repo: PathBuf,
}

impl Source for Repo {
    fn head(&self) -> Result<String, Fault> {
        Ok(self.git.head(&self.repo)?.as_str().to_owned())
    }

    fn tree(&self, commit: &str) -> Result<Vec<(String, String)>, Fault> {
        self.git.tree(&self.repo, &commit_id(commit)?)
    }

    fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault> {
        self.git.blobs(&self.repo, blobs)
    }

    fn changed(&self, path: &Path, commit: &str) -> Result<Vec<(String, Option<String>)>, Fault> {
        let changed = self.git.changed(path, &commit_id(commit)?)?;
        Ok(changed
            .into_iter()
            .map(|file| {
                let text = text_at(&path.join(&file));
                (file, text)
            })
            .collect())
    }
}

/// The text of the regular file at `path`; a link, a folder, a missing file and one that isn't
/// UTF-8 have none.
fn text_at(path: &Path) -> Option<String> {
    let kind = std::fs::symlink_metadata(path).ok()?;
    kind.is_file().then(|| std::fs::read_to_string(path).ok())?
}

fn commit_id(commit: &str) -> Result<CommitId, Fault> {
    CommitId::try_from(commit.to_owned()).map_err(|_| {
        Fault::new(
            code!("map.commit_invalid"),
            "The map's commit isn't a commit",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
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

    #[test]
    fn refuses_a_commit_that_isnt_one() {
        let dir = TempDir::new().unwrap();
        let source = Repo {
            git: git(&dir),
            repo: repo(&dir),
        };
        let fault = source.tree("--output=x").unwrap_err();
        assert_eq!(fault.code().as_str(), "map.commit_invalid");
        let fault = source.changed(&dir, "HEAD").unwrap_err();
        assert_eq!(fault.code().as_str(), "map.commit_invalid");
    }
}
