//! The person's own git, run on their repositories with fixed arguments, no shell, and none of
//! a repository's hooks; filters and helpers the person configured run as in their terminal.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use weacon_observability::Fault;
use weacon_protocol::{ChatId, code};

use crate::platform;
use crate::workspaces::resolved;

mod reads;
pub(crate) mod removal;

// Variables that would point git at another repository, or stand in for `-c` options.
const REDIRECTS: [&str; 9] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_WORK_TREE",
];

/// Git, found on the person's `PATH`. Every call blocks until git exits.
#[derive(Clone, Debug)]
pub(crate) struct Git {
    program: PathBuf,
    no_hooks: PathBuf,
}

/// One of a repository's working trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Worktree {
    pub(crate) path: PathBuf,
    /// The branch checked out, such as `refs/heads/main`; none when its head is detached.
    pub(crate) branch: Option<String>,
}

impl Git {
    /// The first git in `path`, a `PATH` value. `no_hooks`, an empty folder, stands in for every
    /// repository's hooks.
    pub(crate) fn find(path: &OsStr, no_hooks: PathBuf) -> Option<Self> {
        let program = platform::find_program("git", path)?;
        Some(Self { program, no_hooks })
    }

    /// Locks `worktree` against `git worktree prune`, which would forget it while its folder is
    /// in the Trash.
    pub(crate) fn lock(&self, worktree: &Path) -> Result<(), Fault> {
        run(self
            .command(worktree)
            .args(["worktree", "lock", "--reason", "In the Trash"])
            .arg(worktree))
        .map(drop)
    }

    /// Unlocks `worktree` once it's back; one already unlocked stays so.
    pub(crate) fn unlock(&self, worktree: &Path) {
        drop(run(self
            .command(worktree)
            .args(["worktree", "unlock"])
            .arg(worktree)));
    }

    /// Every working tree of `repo` that still has its folder, the main one first.
    pub(crate) fn worktrees(&self, repo: &Path) -> Result<Vec<Worktree>, Fault> {
        Ok(self
            .listed(repo)?
            .into_iter()
            .filter_map(|(tree, missing)| (!missing).then_some(tree))
            .collect())
    }

    /// The worktree at `path` on [`branch`]`(chat)`, made from `repo`'s head if it's missing. A
    /// branch or folder of that name that isn't this chat's worktree is never taken over.
    pub(crate) fn chat_worktree(
        &self,
        repo: &Path,
        chat: &ChatId,
        path: &Path,
    ) -> Result<Worktree, Fault> {
        if !repo.is_absolute() || !path.is_absolute() {
            return Err(Fault::new(
                code!("git.path_relative"),
                "Worktrees need absolute paths",
            ));
        }
        let branch = branch(chat);
        let full = format!("refs/heads/{branch}");
        let listed = self.listed(repo)?;
        let ours = |tree: &Worktree| tree.branch.as_deref() == Some(full.as_str());
        let here = resolved(path).and_then(|path| {
            listed
                .iter()
                .find(|(tree, _)| resolved(&tree.path).as_ref() == Some(&path))
        });
        let fresh = match here {
            Some((tree, false)) if ours(tree) => {
                return Ok(Worktree {
                    path: path.to_owned(),
                    branch: tree.branch.clone(),
                });
            }
            // Its folder was deleted, and no other worktree has its branch: check it out again.
            Some((tree, true))
                if ours(tree) && listed.iter().filter(|(other, _)| ours(other)).count() == 1 =>
            {
                false
            }
            Some(_) => return Err(taken()),
            None if path.symlink_metadata().is_ok() || self.has_branch(repo, &full)? => {
                return Err(taken());
            }
            None => true,
        };
        make_parent(path)?;
        if fresh {
            self.new_branch(repo, path, &branch, &full)?;
        } else {
            self.add(repo, path, &["--force"], &branch)?;
        }
        Ok(Worktree {
            path: path.to_owned(),
            branch: Some(full),
        })
    }

    fn add(&self, repo: &Path, path: &Path, options: &[&str], target: &str) -> Result<(), Fault> {
        run(self
            .command(repo)
            .args(["worktree", "add", "--quiet"])
            .args(options)
            .arg("--")
            .arg(path)
            .arg(target))
        .map(drop)
    }

    // The branch is made only where none has its name, so the one deleted again when the
    // checkout fails is always this call's.
    fn new_branch(&self, repo: &Path, path: &Path, branch: &str, full: &str) -> Result<(), Fault> {
        let head = self.head(repo)?;
        run(self
            .command(repo)
            .args(["update-ref", full, head.as_str(), ""]))
        .map_err(|_| taken())?;
        self.add(repo, path, &[], branch).inspect_err(|_| {
            drop(run(self.command(repo).args([
                "update-ref",
                "-d",
                full,
                head.as_str(),
            ])));
        })
    }

    // Every worktree git knows of, and whether its folder is missing.
    fn listed(&self, repo: &Path) -> Result<Vec<(Worktree, bool)>, Fault> {
        let listed = text(run(self.command(repo).args([
            "worktree",
            "list",
            "--porcelain",
            "-z",
        ]))?)?;
        Ok(listed
            .split("\0\0")
            .filter_map(parse_worktree)
            // A locked worktree's folder can be gone without git calling it prunable.
            .map(|(tree, prunable)| {
                let gone = matches!(
                    tree.path.symlink_metadata(),
                    Err(error) if error.kind() == io::ErrorKind::NotFound
                );
                let missing = prunable || gone;
                (tree, missing)
            })
            .collect())
    }

    fn has_branch(&self, repo: &Path, full: &str) -> Result<bool, Fault> {
        let out = self
            .command(repo)
            .args(["show-ref", "--verify", "--quiet", "--", full])
            .output()
            .map_err(not_run)?;
        // show-ref exits 1 for a missing ref.
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(failed()),
        }
    }

    fn command(&self, repo: &Path) -> Command {
        let mut hooks = OsString::from("core.hooksPath=");
        hooks.push(&self.no_hooks);
        let mut command = Command::new(&self.program);
        command
            .arg("-C")
            .arg(repo)
            .arg("-c")
            .arg(hooks)
            .args(["-c", "core.fsmonitor=false"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        for name in REDIRECTS {
            command.env_remove(name);
        }
        command
    }
}

/// The branch a chat's agents work on, `weacon/` and its id. Ids are tokens, which git takes as
/// branch names as they are.
pub(crate) fn branch(chat: &ChatId) -> String {
    format!("weacon/{}", chat.as_str())
}

// Git's output on success; what it said on stderr is dropped, since it can name files.
fn run(command: &mut Command) -> Result<Vec<u8>, Fault> {
    let out = command.output().map_err(not_run)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(failed())
    }
}

// The folders above a worktree, private to this user.
fn make_parent(path: &Path) -> Result<(), Fault> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    platform::private_dir(parent).map_err(|error| {
        Fault::new(
            code!("git.folder_failed"),
            "The worktree's folder couldn't be created",
        )
        .because(error)
    })
}

fn taken() -> Fault {
    Fault::new(
        code!("git.worktree_taken"),
        "Another branch or folder already has the chat's worktree's name",
    )
}

fn failed() -> Fault {
    Fault::new(code!("git.failed"), "Git couldn't finish")
}

fn not_run(error: io::Error) -> Fault {
    Fault::new(code!("git.not_run"), "Git couldn't be started").because(error)
}

fn text(out: Vec<u8>) -> Result<String, Fault> {
    String::from_utf8(out).map_err(|error| {
        Fault::new(code!("git.unreadable"), "Git's answer wasn't UTF-8").because(error)
    })
}

// One record of `git worktree list --porcelain -z`, and whether git calls it prunable; none for
// a bare repository.
fn parse_worktree(record: &str) -> Option<(Worktree, bool)> {
    let mut path = None;
    let mut branch = None;
    let mut prunable = false;
    for field in record.split('\0') {
        match field.split_once(' ').unwrap_or((field, "")) {
            ("worktree", value) => path = Some(PathBuf::from(value)),
            ("branch", value) => branch = Some(value.to_owned()),
            ("prunable", _) => prunable = true,
            ("bare", _) => return None,
            _ => {}
        }
    }
    Some((
        Worktree {
            path: path?,
            branch,
        },
        prunable,
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    pub(super) fn chat(id: &str) -> ChatId {
        ChatId::try_from(id.to_owned()).unwrap()
    }

    pub(crate) fn git(dir: &Path) -> Git {
        let no_hooks = dir.join("no-hooks");
        fs::create_dir_all(&no_hooks).unwrap();
        Git::find(&std::env::var_os("PATH").unwrap(), no_hooks).unwrap()
    }

    // Runs the fixture's own git in `repo`, without the person's configuration or redirects.
    pub(crate) fn fixture(dir: &Path, repo: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        for name in REDIRECTS {
            command.env_remove(name);
        }
        let done = command
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", dir.join("no-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{args:?} {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    // A repository with one commit.
    pub(crate) fn repo(dir: &Path) -> PathBuf {
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| fixture(dir, &repo, args);
        git(&["init", "--quiet", "--initial-branch=main"]);
        fs::write(repo.join("README.md"), "# Site\n").unwrap();
        // Checked out as it is, whatever the machine's line-ending settings.
        fs::write(repo.join(".gitattributes"), "* -text\n").unwrap();
        git(&["add", "README.md", ".gitattributes"]);
        git(&["commit", "--quiet", "-m", "Start"]);
        repo
    }

    fn real(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[test]
    fn a_chat_gets_a_worktree_on_its_own_branch_and_finds_it_again() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/w-1/c-a");

        let made = git.chat_worktree(&repo, &chat("a-1"), &path).unwrap();
        let again = git.chat_worktree(&repo, &chat("a-1"), &path).unwrap();

        assert_eq!(real(&made.path), real(&path));
        assert_eq!(made.branch.as_deref(), Some("refs/heads/weacon/a-1"));
        assert_eq!(again, made);
        assert_eq!(again.path, path);
        assert_eq!(
            fs::read_to_string(path.join("README.md")).unwrap(),
            "# Site\n"
        );
        assert_eq!(git.worktrees(&repo).unwrap().len(), 2);
    }

    #[test]
    fn worktrees_list_the_main_one_first() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/c-b");
        git.chat_worktree(&repo, &chat("b"), &path).unwrap();

        let trees = git.worktrees(&repo).unwrap();

        assert_eq!(trees.len(), 2);
        let (main, linked) = (&trees[0], &trees[1]);
        assert_eq!(real(&main.path), real(&repo));
        assert_eq!(main.branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(real(&linked.path), real(&path));
    }

    #[test]
    fn a_branch_or_folder_with_the_chats_name_is_never_taken_over() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let first = dir.join("trees/c-c");
        git.chat_worktree(&repo, &chat("c"), &first).unwrap();
        let folder = dir.join("trees/c-d");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("notes.txt"), "mine").unwrap();
        let taken = |chat_id: &str, path: &Path| {
            let error = git.chat_worktree(&repo, &chat(chat_id), path).unwrap_err();
            assert_eq!(error.code().as_str(), "git.worktree_taken");
        };

        // The branch is the chat's, but another folder would get it.
        taken("c", &dir.join("trees/elsewhere"));
        // The folder is the chat's worktree, but for another branch.
        taken("e", &first);
        taken("d", &folder);
        taken("d", &repo);

        assert_eq!(git.worktrees(&repo).unwrap().len(), 2);
        assert_eq!(
            fs::read_to_string(folder.join("notes.txt")).unwrap(),
            "mine"
        );
        assert!(!dir.join("trees/elsewhere").exists());
    }

    #[test]
    fn a_folder_that_isnt_a_repository_fails() {
        let dir = TempDir::new().unwrap();
        let git = git(&dir);

        let error = git.worktrees(&dir).unwrap_err();
        assert_eq!(error.code().as_str(), "git.failed");
        let error = git
            .chat_worktree(&dir, &chat("f"), &dir.join("trees/c-f"))
            .unwrap_err();
        assert_eq!(error.code().as_str(), "git.failed");
    }

    #[cfg(unix)]
    #[test]
    fn the_repositorys_own_hooks_never_run() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let marker = dir.join("hook-ran");
        let hook = repo.join(".git/hooks/post-checkout");
        fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

        git.chat_worktree(&repo, &chat("g"), &dir.join("trees/c-g"))
            .unwrap();

        assert!(!marker.exists());
    }

    #[test]
    fn git_runs_on_the_repository_without_hooks_prompts_or_redirects() {
        let git = Git {
            program: PathBuf::from("git"),
            no_hooks: PathBuf::from("/state/no-hooks"),
        };
        let command = git.command(Path::new("/src/site"));

        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            args.get(..2),
            Some(["-C".as_ref(), "/src/site".as_ref()].as_slice())
        );
        assert!(args.contains(&"core.fsmonitor=false".as_ref()));
        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        for name in REDIRECTS {
            assert!(envs.contains(&(name.as_ref(), None)), "{name}");
        }
        assert!(envs.contains(&("GIT_TERMINAL_PROMPT".as_ref(), Some("0".as_ref()))));
        assert!(envs.contains(&("GIT_OPTIONAL_LOCKS".as_ref(), Some("0".as_ref()))));
    }

    #[test]
    fn git_is_found_only_on_the_path() {
        let dir = TempDir::new().unwrap();
        assert!(Git::find(dir.as_os_str(), dir.join("no-hooks")).is_none());
        let planted = platform::plant_program(&dir, "git", 0o755);
        assert_eq!(
            Git::find(dir.as_os_str(), dir.join("no-hooks"))
                .unwrap()
                .program,
            planted
        );
    }

    #[test]
    fn chats_branches_are_named_after_their_ids() {
        assert_eq!(branch(&chat("01JC2Y-x_1")), "weacon/01JC2Y-x_1");
    }

    #[test]
    fn listed_worktrees_skip_bare_ones_and_mark_missing_ones() {
        let tree = |path: &str, branch: Option<&str>| Worktree {
            path: PathBuf::from(path),
            branch: branch.map(str::to_owned),
        };

        assert_eq!(
            parse_worktree("worktree /t/a\0HEAD 1234\0branch refs/heads/weacon/a"),
            Some((tree("/t/a", Some("refs/heads/weacon/a")), false))
        );
        assert_eq!(
            parse_worktree("worktree /t/b\0HEAD 1234\0detached"),
            Some((tree("/t/b", None), false))
        );
        assert_eq!(
            parse_worktree("worktree /t/c\0HEAD 1234\0branch refs/heads/c\0prunable gone"),
            Some((tree("/t/c", Some("refs/heads/c")), true))
        );
        assert_eq!(parse_worktree("worktree /t/r.git\0bare"), None);
        assert_eq!(parse_worktree(""), None);
    }

    #[test]
    fn a_chat_whose_worktree_folder_was_deleted_gets_it_back_on_its_branch() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/c-h");
        git.chat_worktree(&repo, &chat("h"), &path).unwrap();
        fs::remove_dir_all(&path).unwrap();

        assert_eq!(git.worktrees(&repo).unwrap().len(), 1);
        let back = git.chat_worktree(&repo, &chat("h"), &path).unwrap();

        assert_eq!(back.branch.as_deref(), Some("refs/heads/weacon/h"));
        assert_eq!(
            fs::read_to_string(path.join("README.md")).unwrap(),
            "# Site\n"
        );
        assert_eq!(git.worktrees(&repo).unwrap().len(), 2);
        // Another chat's deleted folder is still not this one's, nor a second home for a branch.
        fs::remove_dir_all(&path).unwrap();
        git.chat_worktree(&repo, &chat("i"), &dir.join("trees/c-i"))
            .unwrap();
        for other in ["i", "m"] {
            let error = git.chat_worktree(&repo, &chat(other), &path).unwrap_err();
            assert_eq!(error.code().as_str(), "git.worktree_taken");
        }
        assert_eq!(git.worktrees(&repo).unwrap().len(), 2);
    }

    #[test]
    fn a_checkout_that_fails_leaves_no_branch_behind() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/c-f");
        // A filter the checkout needs, and that fails.
        fs::write(
            repo.join(".gitattributes"),
            "* -text\nboom.txt filter=boom\n",
        )
        .unwrap();
        fs::write(repo.join("boom.txt"), "boom\n").unwrap();
        fixture(&dir, &repo, &["add", ".gitattributes", "boom.txt"]);
        fixture(&dir, &repo, &["commit", "--quiet", "-m", "Boom"]);
        fixture(&dir, &repo, &["config", "filter.boom.smudge", "false"]);
        fixture(&dir, &repo, &["config", "filter.boom.required", "true"]);

        let error = git.chat_worktree(&repo, &chat("f"), &path).unwrap_err();

        assert_eq!(error.code().as_str(), "git.failed");
        assert!(!git.has_branch(&repo, "refs/heads/weacon/f").unwrap());
        fixture(&dir, &repo, &["config", "--remove-section", "filter.boom"]);
        git.chat_worktree(&repo, &chat("f"), &path).unwrap();
    }

    #[test]
    fn a_branch_made_meanwhile_is_never_taken_back() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let first = dir.join("trees/c-r");
        git.chat_worktree(&repo, &chat("r"), &first).unwrap();
        let full = "refs/heads/weacon/r";

        // As a second call for the chat would, after its check but before its branch.
        let error = git
            .new_branch(&repo, &dir.join("trees/c-r2"), "weacon/r", full)
            .unwrap_err();

        assert_eq!(error.code().as_str(), "git.worktree_taken");
        assert!(git.has_branch(&repo, full).unwrap());
        assert_eq!(
            git.chat_worktree(&repo, &chat("r"), &first).unwrap().path,
            first
        );
    }

    #[test]
    fn a_chat_whose_worktree_and_the_folder_above_were_deleted_gets_it_back() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/w-1/c-p");
        git.chat_worktree(&repo, &chat("p"), &path).unwrap();
        fs::remove_dir_all(dir.join("trees/w-1")).unwrap();

        git.chat_worktree(&repo, &chat("p"), &path).unwrap();

        assert!(path.join("README.md").exists());
    }

    #[test]
    fn a_locked_worktree_whose_folder_is_gone_isnt_listed_or_used() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/c-l");
        git.chat_worktree(&repo, &chat("l"), &path).unwrap();
        fixture(&dir, &repo, &["worktree", "lock", path.to_str().unwrap()]);
        fs::remove_dir_all(&path).unwrap();

        assert_eq!(git.worktrees(&repo).unwrap().len(), 1);
        let error = git.chat_worktree(&repo, &chat("l"), &path).unwrap_err();

        assert_eq!(error.code().as_str(), "git.failed");
        assert!(!path.exists());
    }

    #[test]
    fn relative_paths_are_refused() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let refused = |repo: &Path, path: &Path| {
            let error = git.chat_worktree(repo, &chat("j"), path).unwrap_err();
            assert_eq!(error.code().as_str(), "git.path_relative");
        };

        refused(&repo, Path::new("trees/c-j"));
        refused(Path::new("repo"), &dir.join("trees/c-j"));
        assert!(!dir.join("trees").exists());
    }

    #[test]
    fn a_worktree_whose_folder_cant_be_made_fails() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        fs::write(dir.join("trees"), "a file").unwrap();

        let error = git
            .chat_worktree(&repo, &chat("k"), &dir.join("trees/w/c-k"))
            .unwrap_err();

        assert_eq!(error.code().as_str(), "git.folder_failed");
        assert_eq!(git.worktrees(&repo).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_folders_above_a_worktree_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        git.chat_worktree(&repo, &chat("l"), &dir.join("trees/w-1/c-l"))
            .unwrap();
        let mode = |path: &str| fs::metadata(dir.join(path)).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode("trees"), 0o700);
        assert_eq!(mode("trees/w-1"), 0o700);
    }
}
