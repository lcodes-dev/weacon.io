//! A chat's branch: what its worktree holds that isn't safe yet, and its draft pull request.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use weacon_observability::Fault;
use weacon_protocol::{DownloadUrl, code};

use super::diff::is_commit_id;
use super::{Git, not_run, run, text};
use crate::platform;

// Variables that would sign `gh` in as someone other than the workspace, or point it elsewhere.
const GH_OVERRIDES: [&str; 6] = [
    "GH_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_REPO",
    "GH_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITHUB_TOKEN",
];

/// What removing a worktree would set aside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unsaved {
    /// Changes not committed, untracked files included.
    pub(crate) uncommitted: bool,
    /// Commits since `base` that no remote has.
    pub(crate) unpushed: bool,
}

impl Git {
    /// What `worktree` holds that isn't committed, or is committed since `base` but pushed
    /// nowhere.
    pub(crate) fn unsaved(&self, worktree: &Path, base: &str) -> Result<Unsaved, Fault> {
        if !is_commit_id(base) {
            return Err(Fault::new(
                code!("git.not_a_commit"),
                "That isn't a commit id",
            ));
        }
        let status = run(self.command(worktree).args([
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=normal",
        ]))?;
        let range = format!("{base}..HEAD");
        let ahead = text(run(self.command(worktree).args([
            "rev-list",
            "--count",
            &range,
            "--not",
            "--remotes",
        ]))?)?;
        Ok(Unsaved {
            uncommitted: !status.is_empty(),
            unpushed: ahead.trim_end() != "0",
        })
    }

    /// Pushes `branch` from `worktree` to the repository's `origin`, with the person's own git
    /// and whatever credentials it uses there.
    pub(crate) fn push(&self, worktree: &Path, branch: &str) -> Result<(), Fault> {
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        run(self
            .command(worktree)
            .args(["push", "--quiet", "--set-upstream", "origin", &refspec]))
        .map(drop)
        .map_err(|_| Fault::new(code!("pr.push_failed"), "The branch couldn't be pushed"))
    }

    /// The page where a person can open a pull request of `branch` into `base`, when `origin`
    /// is on GitHub.
    pub(crate) fn compare_page(
        &self,
        worktree: &Path,
        base: &str,
        branch: &str,
    ) -> Result<DownloadUrl, Fault> {
        let remote = text(run(self
            .command(worktree)
            .args(["remote", "get-url", "origin"]))?)?;
        github_repo(remote.trim_end())
            .filter(|_| [base, branch].iter().all(|name| is_plain_branch(name)))
            .and_then(|(owner, repo)| {
                let page =
                    format!("https://github.com/{owner}/{repo}/compare/{base}...{branch}?expand=1");
                DownloadUrl::try_from(page).ok()
            })
            .ok_or_else(|| {
                Fault::new(
                    code!("pr.no_page"),
                    "There's no page to open a pull request on for this repository",
                )
            })
    }
}

/// A draft pull request to open: of `branch`, already pushed, into `base`.
#[derive(Debug)]
pub(crate) struct Draft<'a> {
    pub(crate) base: &'a str,
    pub(crate) branch: &'a str,
    pub(crate) title: &'a str,
    pub(crate) body: &'a str,
}

/// The person's own `gh`, signed in with the workspace's own config folder and nothing else.
#[derive(Debug)]
pub(crate) struct Gh {
    program: PathBuf,
    config: PathBuf,
}

impl Gh {
    /// The first `gh` in `path`, a `PATH` value, using `config` for its sign-in.
    pub(crate) fn find(path: &OsStr, config: PathBuf) -> Option<Self> {
        let program = platform::find_program("gh", path)?;
        Some(Self { program, config })
    }

    /// Whether the workspace is signed in to GitHub with `gh`; only the answer is read.
    pub(crate) fn signed_in(&self, worktree: &Path) -> bool {
        self.command(worktree)
            .args(["auth", "status", "--hostname", "github.com"])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Opens `draft` and gives its page.
    pub(crate) fn open_draft(
        &self,
        worktree: &Path,
        draft: &Draft<'_>,
    ) -> Result<DownloadUrl, Fault> {
        let Draft {
            base,
            branch,
            title,
            body,
        } = draft;
        if title.trim().is_empty() {
            return Err(Fault::new(
                code!("pr.no_title"),
                "A pull request needs a title",
            ));
        }
        let mut child = self
            .command(worktree)
            .args(["pr", "create", "--draft", "--body-file", "-"])
            .arg(format!("--base={base}"))
            .arg(format!("--head={branch}"))
            .arg(format!("--title={title}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(not_run)?;
        if let Some(mut input) = child.stdin.take() {
            // A body gh didn't read shows as gh's own failure below.
            drop(input.write_all(body.as_bytes()));
        }
        let out = child.wait_with_output().map_err(not_run)?;
        let opened = || {
            Fault::new(
                code!("pr.not_opened"),
                "The pull request couldn't be opened",
            )
        };
        if !out.status.success() {
            return Err(opened());
        }
        let page = String::from_utf8(out.stdout).map_err(|_| opened())?;
        page.lines()
            .rev()
            .find_map(|line| DownloadUrl::try_from(line.trim().to_owned()).ok())
            .ok_or_else(opened)
    }

    fn command(&self, worktree: &Path) -> Command {
        let mut command = Command::new(&self.program);
        command
            .current_dir(worktree)
            .env("GH_CONFIG_DIR", &self.config)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        for name in GH_OVERRIDES {
            command.env_remove(name);
        }
        command
    }
}

// A branch name that reads the same in a page's address.
fn is_plain_branch(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
}

// The owner and name of a repository on GitHub, from a remote's address in any of git's
// forms; any user name or password in it is dropped.
fn github_repo(remote: &str) -> Option<(String, String)> {
    let rest = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| {
            let after = remote.strip_prefix("https://")?;
            let host_on = after.rsplit_once('@').map_or(after, |(_, host)| host);
            host_on.strip_prefix("github.com/")
        })?;
    let (owner, repo) = rest.trim_end_matches('/').split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    (plain(owner) && plain(repo)).then(|| (owner.to_owned(), repo.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, fixture, git, repo};

    // A chat's worktree whose repository's `origin` is a bare one beside it, and its start.
    fn with_origin(dir: &Path) -> (Git, PathBuf, PathBuf, String) {
        let (git, repo) = (git(dir), repo(dir));
        let origin = dir.join("origin.git");
        fixture(
            dir,
            dir,
            &["init", "--quiet", "--bare", origin.to_str().unwrap()],
        );
        fixture(
            dir,
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        fixture(dir, &repo, &["push", "--quiet", "origin", "main"]);
        let path = dir.join("trees/c-p");
        let start = git
            .chat_worktree(&repo, &chat("p"), &path)
            .unwrap()
            .start
            .unwrap();
        (git, path, origin, start.commit)
    }

    #[test]
    fn a_worktree_holds_unsaved_work_until_it_is_committed_and_pushed() {
        let dir = TempDir::new().unwrap();
        let (git, path, origin, base) = with_origin(&dir);
        let unsaved = |uncommitted, unpushed| Unsaved {
            uncommitted,
            unpushed,
        };

        assert_eq!(git.unsaved(&path, &base).unwrap(), unsaved(false, false));
        fs::write(path.join("agent.txt"), "mine\n").unwrap();
        assert_eq!(git.unsaved(&path, &base).unwrap(), unsaved(true, false));
        fixture(&dir, &path, &["add", "agent.txt"]);
        fixture(&dir, &path, &["commit", "--quiet", "-m", "Agent"]);
        assert_eq!(git.unsaved(&path, &base).unwrap(), unsaved(false, true));

        git.push(&path, "weacon/p").unwrap();

        assert_eq!(git.unsaved(&path, &base).unwrap(), unsaved(false, false));
        fixture(
            &dir,
            &origin,
            &["rev-parse", "--verify", "--quiet", "refs/heads/weacon/p"],
        );
        let error = git.unsaved(&path, "HEAD").unwrap_err();
        assert_eq!(error.code().as_str(), "git.not_a_commit");
    }

    #[test]
    fn a_branch_without_somewhere_to_go_isnt_pushed() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/c-q");
        git.chat_worktree(&repo, &chat("q"), &path).unwrap();

        let error = git.push(&path, "weacon/q").unwrap_err();

        assert_eq!(error.code().as_str(), "pr.push_failed");
    }

    #[test]
    fn a_github_remote_gives_the_page_to_open_a_pull_request_on() {
        let dir = TempDir::new().unwrap();
        let (git, repo) = (git(&dir), repo(&dir));
        let path = dir.join("trees/c-g");
        git.chat_worktree(&repo, &chat("g"), &path).unwrap();
        let remote = "https://ada:token@github.com/acme/site.git";
        fixture(&dir, &repo, &["remote", "add", "origin", remote]);

        let page = git.compare_page(&path, "main", "weacon/g").unwrap();

        assert_eq!(
            page.as_str(),
            "https://github.com/acme/site/compare/main...weacon/g?expand=1"
        );
        let odd = git.compare_page(&path, "main#1", "weacon/g").unwrap_err();
        assert_eq!(odd.code().as_str(), "pr.no_page");
    }

    #[test]
    fn remotes_are_read_in_each_of_gits_forms_and_only_on_github() {
        let site = Some(("acme".to_owned(), "site".to_owned()));
        for remote in [
            "git@github.com:acme/site.git",
            "ssh://git@github.com/acme/site",
            "https://github.com/acme/site/",
            "https://ada:secret@github.com/acme/site.git",
        ] {
            assert_eq!(github_repo(remote), site, "{remote}");
        }
        for remote in [
            "https://gitlab.com/acme/site.git",
            "https://github.com/acme",
            "https://github.com/acme/si te",
            "/srv/site.git",
        ] {
            assert_eq!(github_repo(remote), None, "{remote}");
        }
    }

    #[test]
    fn gh_runs_with_the_workspaces_sign_in_and_no_one_elses() {
        let gh = Gh {
            program: PathBuf::from("gh"),
            config: PathBuf::from("/state/w-1/gh"),
        };
        let command = gh.command(Path::new("/trees/c-1"));

        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        assert!(envs.contains(&("GH_CONFIG_DIR".as_ref(), Some("/state/w-1/gh".as_ref()))));
        assert!(envs.contains(&("GH_PROMPT_DISABLED".as_ref(), Some("1".as_ref()))));
        for name in GH_OVERRIDES {
            assert!(envs.contains(&(name.as_ref(), None)), "{name}");
        }
        assert_eq!(command.get_current_dir(), Some(Path::new("/trees/c-1")));
        let draft = Draft {
            base: "main",
            branch: "weacon/c-1",
            title: " ",
            body: "",
        };
        let error = gh.open_draft(Path::new("/trees/c-1"), &draft).unwrap_err();
        assert_eq!(error.code().as_str(), "pr.no_title");
    }

    #[test]
    fn gh_is_found_only_on_the_path() {
        let dir = TempDir::new().unwrap();
        assert!(Gh::find(dir.as_os_str(), dir.join("gh")).is_none());
        let planted = platform::plant_program(&dir, "gh", 0o755);
        let gh = Gh::find(dir.as_os_str(), dir.join("gh")).unwrap();
        assert_eq!(gh.program, planted);
        // A gh that won't run is no sign-in.
        assert!(!gh.signed_in(&dir));
    }
}
