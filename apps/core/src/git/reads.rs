//! What the code map reads from a repository: commits, trees, blobs and what a worktree
//! changed. Every call only reads.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use weacon_observability::Fault;
use weacon_protocol::{CommitId, code};

use super::{Git, failed, not_run, run, text};

/// A regular file in a commit's tree: its path from the repository's root, its blob and its
/// size in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeFile {
    pub(crate) path: String,
    pub(crate) blob: String,
    pub(crate) size: u64,
}

impl Git {
    /// The commit `repo`'s worktree has checked out.
    pub(crate) fn head(&self, repo: &Path) -> Result<CommitId, Fault> {
        let out = run(self
            .command(repo)
            .args(["rev-parse", "--verify", "HEAD^{commit}"]))?;
        CommitId::try_from(text(out)?.trim().to_owned()).map_err(|_| unexpected())
    }

    /// Each regular file at `commit`, links left out; paths that aren't UTF-8 are left out.
    pub(crate) fn tree(&self, repo: &Path, commit: &CommitId) -> Result<Vec<TreeFile>, Fault> {
        let out = run(self
            .command(repo)
            .args(["ls-tree", "-r", "-z", "--long", "--full-tree"])
            .arg(commit.as_str()))?;
        Ok(out
            .split(|byte| *byte == 0)
            .filter_map(|record| {
                let record = std::str::from_utf8(record).ok()?;
                let (about, path) = record.split_once('\t')?;
                let mut about = about.split_whitespace();
                let (mode, kind, blob) = (about.next()?, about.next()?, about.next()?);
                let size = about.next()?.parse().ok()?;
                (kind == "blob" && matches!(mode, "100644" | "100755")).then(|| TreeFile {
                    path: path.to_owned(),
                    blob: blob.to_owned(),
                    size,
                })
            })
            .collect())
    }

    /// The text of each of `blobs` that is UTF-8, by blob. Ids go to git on its input, never
    /// on its command line.
    pub(crate) fn blobs(
        &self,
        repo: &Path,
        blobs: &[String],
    ) -> Result<BTreeMap<String, String>, Fault> {
        if blobs.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut child = self
            .command(repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(not_run)?;
        let input: String = blobs
            .iter()
            .flat_map(|blob| [blob.as_str(), "\n"])
            .collect();
        let mut stdin = child.stdin.take().ok_or_else(failed)?;
        // Written beside the read, so neither side waits on a full pipe.
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let out = child.wait_with_output().map_err(not_run)?;
        writer.join().map_err(|_| failed())?.map_err(not_run)?;
        if !out.status.success() {
            return Err(failed());
        }
        Ok(batch(&out.stdout))
    }

    /// The files whose text in the worktree at `worktree` differs from `commit`, committed or
    /// not, and those git doesn't track and doesn't ignore; paths that aren't UTF-8 are left out.
    pub(crate) fn changed(&self, worktree: &Path, commit: &CommitId) -> Result<Vec<String>, Fault> {
        let changed = run(self
            .command(worktree)
            .args(["diff", "--name-only", "-z", "--no-renames"])
            .arg(commit.as_str())
            .arg("--"))?;
        let untracked =
            run(self
                .command(worktree)
                .args(["ls-files", "-z", "--others", "--exclude-standard"]))?;
        let mut paths: Vec<String> = [changed, untracked]
            .iter()
            .flat_map(|out| out.split(|byte| *byte == 0))
            .filter_map(|path| std::str::from_utf8(path).ok())
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// The UTF-8 blobs in `cat-file --batch` output, by id: each a header `<id> <type> <size>`, then
/// that many bytes and a newline.
fn batch(mut out: &[u8]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    while let Some(end) = out.iter().position(|byte| *byte == b'\n') {
        let header = std::str::from_utf8(out.get(..end).unwrap_or_default()).unwrap_or_default();
        let mut fields = header.split(' ');
        let (id, kind, size) = (fields.next(), fields.next(), fields.next());
        // A missing object has a header alone; anything else has its bytes and a newline.
        let missing = kind == Some("missing");
        let size = if missing {
            Some(0)
        } else {
            size.and_then(|size| size.parse::<usize>().ok())
        };
        let start = end + 1;
        let body = size.and_then(|size| out.get(start..start.checked_add(size)?));
        let Some(body) = body else {
            break;
        };
        if let (Some(id), Some("blob"), Ok(text)) = (id, kind, std::str::from_utf8(body)) {
            found.insert(id.to_owned(), text.to_owned());
        }
        out = out
            .get(start + body.len() + usize::from(!missing)..)
            .unwrap_or_default();
    }
    found
}

fn unexpected() -> Fault {
    Fault::new(
        code!("git.unexpected"),
        "Git's answer wasn't what was expected",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{fixture, git, repo};

    #[test]
    fn reads_the_head_its_tree_and_its_blobs() {
        let dir = TempDir::new().unwrap();
        let repo = repo(&dir);
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "fn a() {}\n").unwrap();
        fs::write(repo.join("bytes.bin"), [0xff, 0xfe, b'\n']).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("README.md", repo.join("link.md")).unwrap();
        fixture(&dir, &repo, &["add", "--all"]);
        fixture(&dir, &repo, &["commit", "--quiet", "-m", "Add"]);
        let git = git(&dir);

        let head = git.head(&repo).unwrap();
        let tree = git.tree(&repo, &head).unwrap();
        let paths: Vec<&str> = tree.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [".gitattributes", "README.md", "bytes.bin", "src/lib.rs"]
        );

        let lib = tree.iter().find(|file| file.path == "src/lib.rs").unwrap();
        assert_eq!(lib.size, 10);
        let mut ids: Vec<String> = tree.iter().map(|file| file.blob.clone()).collect();
        ids.push("0".repeat(40));
        let texts = git.blobs(&repo, &ids).unwrap();
        let by_path = |path: &str| {
            let file = tree.iter().find(|file| file.path == path).unwrap();
            texts.get(&file.blob).cloned()
        };
        assert_eq!(by_path("src/lib.rs").as_deref(), Some("fn a() {}\n"));
        assert_eq!(by_path("README.md").as_deref(), Some("# Site\n"));
        assert_eq!(by_path("bytes.bin"), None);
        assert_eq!(texts.len(), 3);
        assert!(git.blobs(&repo, &[]).unwrap().is_empty());
    }

    #[test]
    fn lists_what_a_worktree_changed_from_a_commit() {
        let dir = TempDir::new().unwrap();
        let repo = repo(&dir);
        let git = git(&dir);
        let start = git.head(&repo).unwrap();
        fs::write(repo.join("committed.md"), "x").unwrap();
        fixture(&dir, &repo, &["add", "committed.md"]);
        fixture(&dir, &repo, &["commit", "--quiet", "-m", "More"]);
        fs::write(repo.join("README.md"), "# Changed\n").unwrap();
        fs::write(repo.join("new.md"), "y").unwrap();
        fs::write(repo.join(".gitignore"), "ignored.md\n").unwrap();
        fs::write(repo.join("ignored.md"), "z").unwrap();
        fs::write(repo.join("-n"), "looks like an option").unwrap();

        let changed = git.changed(&repo, &start).unwrap();
        assert_eq!(
            changed,
            ["-n", ".gitignore", "README.md", "committed.md", "new.md"]
        );
    }

    #[test]
    fn a_folder_that_isnt_a_repository_fails() {
        let dir = TempDir::new().unwrap();
        let git = git(&dir);
        assert_eq!(git.head(&dir).unwrap_err().code().as_str(), "git.failed");
    }

    #[test]
    fn reads_a_batch_by_its_headers() {
        let out = b"a blob 2\nhi\nb missing\nc tree 3\nabc\nd blob 1\n\xff\ne blob 3\nyes\n";
        let found = batch(out);
        let found: Vec<(&str, &str)> = found
            .iter()
            .map(|(id, text)| (id.as_str(), text.as_str()))
            .collect();
        assert_eq!(found, [("a", "hi"), ("e", "yes")]);
        for cut in [
            &b"a blob 2\nhi\nb blob 9\nshort\n"[..],
            b"a blob 2\nhi\nb blob 18446744073709551615\nx\n",
            b"a blob 2\nhi\nb blob two\nab\n",
        ] {
            assert_eq!(batch(cut).into_keys().collect::<Vec<_>>(), ["a"]);
        }
    }
}
