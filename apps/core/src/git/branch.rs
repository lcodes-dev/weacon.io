//! A chat's branch: what its worktree holds that isn't safe yet, and its draft pull request.

use std::ffi::OsStr;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use weacon_observability::Fault;
use weacon_protocol::{DownloadUrl, code};

use super::diff::CommitId;
use super::{Git, REDIRECTS, not_run, run, text};
use crate::platform;

// A push and each gh call reach the network: past these they're stopped.
const PUSH_LIMIT: Duration = Duration::from_mins(2);
const GH_LIMIT: Duration = Duration::from_mins(1);
const POLL: Duration = Duration::from_millis(20);

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
    pub(crate) fn unsaved(&self, worktree: &Path, base: &CommitId) -> Result<Unsaved, Fault> {
        let status = run(self.command(worktree).args([
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=normal",
        ]))?;
        let range = format!("{}..HEAD", base.as_str());
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
        let mut command = self.command(worktree);
        command
            .args(["push", "--quiet", "--set-upstream", "origin", &refspec])
            .stderr(Stdio::null());
        output_for(&mut command, PUSH_LIMIT)
            .map(drop)
            .ok_or_else(|| Fault::new(code!("pr.push_failed"), "The branch couldn't be pushed"))
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
    pub(crate) fn find(path: Option<&OsStr>, config: PathBuf) -> Option<Self> {
        let program = platform::find_program("gh", path.unwrap_or_default())?;
        Some(Self { program, config })
    }

    /// Whether the workspace is signed in to GitHub with `gh`; only the answer is read.
    pub(crate) fn signed_in(&self, worktree: &Path) -> bool {
        let mut command = self.command(worktree);
        command.args(["auth", "status", "--hostname", "github.com"]);
        output_for(&mut command, GH_LIMIT).is_some()
    }

    /// Opens `draft` and gives its page; one already open for its branch is given instead.
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
        if let Some(open) = self.open_for(worktree, branch) {
            return Ok(open);
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
        let opened = || {
            Fault::new(
                code!("pr.not_opened"),
                "The pull request couldn't be opened",
            )
        };
        let status = wait_for(&mut child, GH_LIMIT).map_err(not_run)?;
        if !status.is_some_and(|status| status.success()) {
            return Err(opened());
        }
        read_out(&mut child).and_then(last_page).ok_or_else(opened)
    }

    // The page of the pull request already open for `branch`, if gh can say.
    fn open_for(&self, worktree: &Path, branch: &str) -> Option<DownloadUrl> {
        let mut command = self.command(worktree);
        command
            .args(["pr", "list", "--state=open", "--json=url", "--jq=.[].url"])
            .arg(format!("--head={branch}"));
        output_for(&mut command, GH_LIMIT).and_then(last_page)
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
        // Nor another repository's state, which git's own variables would point it at.
        for name in GH_OVERRIDES.iter().chain(&REDIRECTS) {
            command.env_remove(name);
        }
        command
    }
}

// What `command` printed, when it succeeds within `limit`. Its output is read once it ends, so
// it must be small: the little a push or gh prints.
fn output_for(command: &mut Command, limit: Duration) -> Option<Vec<u8>> {
    let mut child = command.stdout(Stdio::piped()).spawn().ok()?;
    let status = wait_for(&mut child, limit).ok()??;
    status.success().then_some(())?;
    read_out(&mut child)
}

fn read_out(child: &mut Child) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    Some(out)
}

// How `child` ended, or None once `limit` passes, when it is stopped.
fn wait_for(child: &mut Child, limit: Duration) -> io::Result<Option<ExitStatus>> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if start.elapsed() >= limit {
            // It may have just ended; either way it is waited for.
            drop(child.kill());
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

// The last line of gh's output that is a page's address.
fn last_page(out: Vec<u8>) -> Option<DownloadUrl> {
    String::from_utf8(out)
        .ok()?
        .lines()
        .rev()
        .find_map(|line| DownloadUrl::try_from(line.trim().to_owned()).ok())
}

// A branch name that reads the same in a page's address.
fn is_plain_branch(name: &str) -> bool {
    plain(name, b"._/-")
}

// Not empty, and only ASCII letters, digits and the bytes in `also`.
fn plain(text: &str, also: &[u8]) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || also.contains(&byte))
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
    (plain(owner, b"._-") && plain(repo, b"._-")).then(|| (owner.to_owned(), repo.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, fixture, git, repo};

    // A chat's worktree whose repository's `origin` is a bare one beside it, and its start.
    fn with_origin(dir: &Path) -> (Git, PathBuf, PathBuf, CommitId) {
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
        (git, path, origin, CommitId::parse(&start.commit).unwrap())
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
        for name in GH_OVERRIDES.iter().chain(&REDIRECTS) {
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

    // Signed in; notes its arguments one to a line, and lists the draft once it's opened.
    #[cfg(unix)]
    const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$@" >> "$GH_CONFIG_DIR/args"
case "$1 $2" in
"auth status") exit 0 ;;
"pr list") cat "$GH_CONFIG_DIR/open" 2>/dev/null; exit 0 ;;
"pr create")
    cat > "$GH_CONFIG_DIR/body"
    printf 'Creating draft\nhttps://github.com/acme/site/pull/7\n' | tee "$GH_CONFIG_DIR/open"
    ;;
*) exit 1 ;;
esac
"#;

    #[cfg(unix)]
    #[test]
    fn a_draft_is_opened_once_with_each_value_one_argument() {
        let dir = TempDir::new().unwrap();
        let config = dir.join("gh");
        fs::create_dir_all(&config).unwrap();
        let program = dir.join("bin/gh");
        fs::create_dir_all(dir.join("bin")).unwrap();
        weacon_testing::platform::write_program(&program, FAKE_GH).unwrap();
        let gh = Gh {
            program,
            config: config.clone(),
        };
        let draft = Draft {
            base: "main",
            branch: "weacon/c-1",
            title: "Fix it --draft",
            body: "Line one\n--title=x\n",
        };
        assert!(gh.signed_in(&dir));

        let first = gh.open_draft(&dir, &draft).unwrap();
        let again = gh.open_draft(&dir, &draft).unwrap();

        assert_eq!(first.as_str(), "https://github.com/acme/site/pull/7");
        assert_eq!(again, first);
        assert_eq!(fs::read_to_string(config.join("body")).unwrap(), draft.body);
        let args = fs::read_to_string(config.join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(args.iter().filter(|arg| **arg == "create").count(), 1);
        for arg in ["--base=main", "--head=weacon/c-1", "--title=Fix it --draft"] {
            assert!(args.contains(&arg), "{arg}: {args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_past_its_time_is_stopped() {
        let mut slow = Command::new("sleep").arg("30").spawn().unwrap();
        let started = Instant::now();

        assert_eq!(
            wait_for(&mut slow, Duration::from_millis(50)).unwrap(),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(slow.try_wait().unwrap().is_some());

        let mut quick = Command::new("true");
        assert_eq!(
            output_for(&mut quick, Duration::from_secs(30)),
            Some(Vec::new())
        );
        assert_eq!(
            output_for(&mut Command::new("false"), Duration::from_secs(30)),
            None
        );
    }

    #[test]
    fn gh_is_found_only_on_the_path() {
        let dir = TempDir::new().unwrap();
        assert!(Gh::find(Some(dir.as_os_str()), dir.join("gh")).is_none());
        let planted = platform::plant_program(&dir, "gh", 0o755);
        let gh = Gh::find(Some(dir.as_os_str()), dir.join("gh")).unwrap();
        assert_eq!(gh.program, planted);
        // A gh that won't run is no sign-in.
        assert!(!gh.signed_in(&dir));
    }
}
