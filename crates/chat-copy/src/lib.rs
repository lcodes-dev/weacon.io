//! A chat's copy of its repository on a machine: fetched once, compared with where it
//! started, and pushed. The agent may write anywhere in the copy, so git's own folder is
//! kept beside it, where no hook or setting of the agent's making can reach.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use core_link::{Changed, Repository};
use tokio::process::Command;

/// The commit a copy started from.
const BASE: &str = "refs/weacon/base";
/// A longer diff is left out, and the files' counts have to do.
const LONGEST: usize = 400_000;

/// Where the chat's files are, which is the folder its agent works in.
pub fn folder(chat: &Path) -> PathBuf {
    chat.join("repository")
}

/// Where the chat's commands may keep what they download, which is no part of its repository.
pub fn cache(chat: &Path) -> PathBuf {
    chat.join("cache")
}

/// Runs git on the chat's copy and returns what it prints, or what it complains of.
async fn git(chat: &Path, args: &[&str], token: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("--git-dir").arg(chat.join("git"));
    command.arg("--work-tree").arg(folder(chat));
    command.args(["-c", "core.quotePath=false"]).args(args);
    command.current_dir(chat).stdin(Stdio::null());
    // None of the person's own settings, so that nothing asks, signs or runs a helper.
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_TERMINAL_PROMPT", "0");
    // An address may name a program to run, in other protocols.
    command.env("GIT_ALLOW_PROTOCOL", "https:file");
    if let Some(token) = token {
        let basic = BASE64.encode(format!("x-access-token:{token}"));
        // Given in the environment, so that it is in no file and no list of running programs.
        command.env("GIT_CONFIG_COUNT", "1");
        command.env("GIT_CONFIG_KEY_0", "http.extraHeader");
        let header = format!("Authorization: Basic {basic}");
        command.env("GIT_CONFIG_VALUE_0", header);
    }
    let output = command.kill_on_drop(true).output().await;
    let output = output.map_err(|error| format!("git could not be started: {error}"))?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        false => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
}

/// Makes the chat's copy of its repository, at the branch's newest commit, unless it is there.
pub async fn fetch(chat: &Path, repository: &Repository) -> Result<(), String> {
    let copy = folder(chat);
    if chat.join("git").exists() {
        return Ok(());
    }
    let made = tokio::fs::create_dir_all(&copy).await;
    made.map_err(|error| error.to_string())?;
    let Repository { url, branch, token } = repository;
    let fetched = async {
        git(chat, &["init", "--quiet"], None).await?;
        let fetch = ["fetch", "--quiet", "--depth", "1", url, branch];
        git(chat, &fetch, Some(token)).await?;
        git(chat, &["update-ref", BASE, "FETCH_HEAD"], None).await?;
        let checkout = ["checkout", "--quiet", "-B", branch, "FETCH_HEAD"];
        git(chat, &checkout, None).await?;
        // For the agent's own `git status` and `git diff`. Nothing here reads it.
        let pointer = tokio::fs::write(copy.join(".git"), "gitdir: ../git\n").await;
        pointer.map_err(|error| error.to_string())
    };
    if let Err(why) = fetched.await {
        // A half-made copy would pass for a whole one next time.
        tokio::fs::remove_dir_all(chat.join("git")).await.ok();
        tokio::fs::remove_dir_all(copy).await.ok();
        return Err(why);
    }
    Ok(())
}

/// The files that differ from the commit the copy started from, new ones included, and
/// the diff of them all, which is empty when it is too long to show.
pub async fn changes(chat: &Path) -> (Vec<Changed>, String) {
    let added = git(chat, &["add", "--all", "--intent-to-add"], None).await;
    added.ok();
    let plain = ["--no-renames", "--no-ext-diff", "--no-textconv", BASE];
    let diff = |how| async move {
        let asked = [&["diff", how][..], &plain[..]].concat();
        git(chat, &asked, None).await.unwrap_or_default()
    };
    let counted = diff("--numstat").await;
    let files = counted.lines().filter_map(|line| {
        let mut parts = line.splitn(3, '\t');
        // A file that isn't text is counted as "-".
        let mut count = || parts.next().map(|count| count.parse().unwrap_or(0));
        let (added, removed) = (count()?, count()?);
        let path = parts.next()?.to_owned();
        Some(Changed {
            path,
            added,
            removed,
        })
    });
    let diff = diff("--no-color").await;
    let diff = if diff.len() > LONGEST {
        String::new()
    } else {
        diff
    };
    (files.collect(), diff)
}

/// What the person's own git on this machine calls them, if it knows both.
async fn own() -> Option<(String, String)> {
    let known = |key: &'static str| async move {
        let asked = Command::new("git")
            .args(["config", "--global", "--get", key])
            .stdin(Stdio::null())
            .output();
        let value = String::from_utf8(asked.await.ok()?.stdout).ok()?;
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    };
    Some((known("user.name").await?, known("user.email").await?))
}

/// Commits whatever the copy holds changed, by the person as their own git knows them or
/// else as `author`, a name and a mail. Then pushes to the branch `to`, also when there
/// was nothing to commit, so that asking twice is safe. Returns the commit.
pub async fn push(
    chat: &Path,
    repository: &Repository,
    to: &str,
    message: &str,
    author: (&str, &str),
) -> Result<String, String> {
    git(chat, &["add", "--all"], None).await?;
    if git(chat, &["diff", "--cached", "--quiet", "HEAD"], None)
        .await
        .is_err()
    {
        let own = own().await;
        let (name, mail) = match &own {
            Some((name, mail)) => (name.as_str(), mail.as_str()),
            None => author,
        };
        let (name, mail) = (format!("user.name={name}"), format!("user.email={mail}"));
        let commit = ["-c", &name, "-c", &mail, "commit", "--quiet", "--no-verify"];
        let commit = [&commit[..], &["-m", message]].concat();
        git(chat, &commit, None).await?;
    }
    let to = format!("HEAD:refs/heads/{to}");
    let push = ["push", "--quiet", &repository.url, &to];
    git(chat, &push, Some(&repository.token)).await?;
    Ok(git(chat, &["rev-parse", "HEAD"], None)
        .await?
        .trim()
        .to_owned())
}
