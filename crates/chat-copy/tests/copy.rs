//! A chat's copy, against a repository made in a temporary folder.

use std::path::{Path, PathBuf};
use std::process::Command;

use core_link::{Changed, Repository};

fn git(folder: &Path, args: &[&str]) -> String {
    let run = Command::new("git").args(args).current_dir(folder).output();
    let run = run.expect("git runs");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout).unwrap().trim().to_owned()
}

/// A repository with one commit on `main`, and an empty folder for a chat.
fn made(name: &str) -> (PathBuf, Repository) {
    let root = std::env::temp_dir().join(format!("chat-copy-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let origin = root.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--quiet", "--initial-branch", "main"]);
    std::fs::write(origin.join("kept.txt"), "one\ntwo\n").unwrap();
    std::fs::write(origin.join("gone.txt"), "old\n").unwrap();
    git(&origin, &["add", "--all"]);
    let commit = ["-c", "user.name=A", "-c", "user.email=a@example.com"];
    git(
        &origin,
        &[&commit[..], &["commit", "--quiet", "-m", "first"]].concat(),
    );
    let repository = Repository {
        url: origin.to_str().unwrap().to_owned(),
        branch: "main".into(),
        token: "a-token".into(),
    };
    (root.join("chat"), repository)
}

fn file(path: &str, added: u32, removed: u32) -> Changed {
    let path = path.to_owned();
    Changed {
        path,
        added,
        removed,
    }
}

#[tokio::test]
async fn a_copy_is_fetched_and_says_what_changed_in_it() {
    let (chat, repository) = made("changes");
    chat_copy::fetch(&chat, &repository).await.unwrap();
    let copy = chat_copy::folder(&chat);
    assert_eq!(
        std::fs::read_to_string(copy.join("kept.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(chat_copy::changes(&chat).await, (vec![], String::new()));
    // The agent's own git reads the copy.
    assert_eq!(git(&copy, &["status", "--porcelain"]), "");

    std::fs::write(copy.join("kept.txt"), "one\nthree\n").unwrap();
    std::fs::write(copy.join("new file.txt"), "new\n").unwrap();
    std::fs::remove_file(copy.join("gone.txt")).unwrap();
    // Fetching again leaves the copy as it is.
    chat_copy::fetch(&chat, &repository).await.unwrap();
    let (files, diff) = chat_copy::changes(&chat).await;
    let expected = [
        file("gone.txt", 0, 1),
        file("kept.txt", 1, 1),
        file("new file.txt", 1, 0),
    ];
    assert_eq!(files, expected);
    assert!(diff.contains("diff --git a/kept.txt b/kept.txt"), "{diff}");
    assert!(diff.contains("-two\n+three\n"), "{diff}");
    assert!(diff.contains("+++ b/new file.txt"), "{diff}");

    std::fs::write(copy.join("long.txt"), "long line\n".repeat(50_000)).unwrap();
    let (files, diff) = chat_copy::changes(&chat).await;
    assert_eq!(files.len(), 4);
    assert_eq!(diff, "");
}

#[tokio::test]
async fn a_copy_is_committed_and_pushed_to_a_branch() {
    let (chat, repository) = made("push");
    let origin = PathBuf::from(&repository.url);
    chat_copy::fetch(&chat, &repository).await.unwrap();
    let copy = chat_copy::folder(&chat);
    std::fs::write(copy.join("kept.txt"), "one\nthree\n").unwrap();
    let author = ("Ada Dorn", "ada@example.com");
    let pushed = chat_copy::push(&chat, &repository, "weacon/a-change", "Change two", author);
    let first = pushed.await.unwrap();
    assert_eq!(git(&origin, &["rev-parse", "weacon/a-change"]), first);
    assert_eq!(
        git(&origin, &["log", "-1", "--format=%s", &first]),
        "Change two"
    );
    assert_eq!(
        git(&origin, &["rev-parse", &format!("{first}^")]),
        git(&origin, &["rev-parse", "main"])
    );
    // The person's own git names the author where it knows them, and what was given otherwise.
    let own = |key| {
        let known = Command::new("git")
            .args(["config", "--global", "--get", key])
            .output();
        String::from_utf8(known.unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned()
    };
    let (name, mail) = (own("user.name"), own("user.email"));
    let by = if name.is_empty() || mail.is_empty() {
        "Ada Dorn <ada@example.com>".to_owned()
    } else {
        format!("{name} <{mail}>")
    };
    assert_eq!(
        git(&origin, &["log", "-1", "--format=%an <%ae>", &first]),
        by
    );

    // With nothing new it pushes the same commit again.
    let again = chat_copy::push(&chat, &repository, "weacon/a-change", "Nothing", author);
    assert_eq!(again.await.unwrap(), first);
    // What the chat changed is still told against where it started.
    assert_eq!(chat_copy::changes(&chat).await.0, [file("kept.txt", 1, 1)]);

    std::fs::write(copy.join("more.txt"), "more\n").unwrap();
    let second = chat_copy::push(&chat, &repository, "weacon/a-change", "More", author);
    let second = second.await.unwrap();
    assert_eq!(git(&origin, &["rev-parse", "weacon/a-change"]), second);
    assert_eq!(git(&origin, &["rev-parse", &format!("{second}^")]), first);
}

#[tokio::test]
async fn what_an_agent_writes_into_the_copy_s_git_is_not_run() {
    let (chat, repository) = made("hooks");
    chat_copy::fetch(&chat, &repository).await.unwrap();
    let copy = chat_copy::folder(&chat);
    let ran = chat.join("ran");
    let script = format!("#!/bin/sh\ntouch '{}'\n", ran.display());
    std::fs::write(copy.join("hook.sh"), &script).unwrap();
    // In place of the pointer, a folder with hooks, and settings that name a program.
    std::fs::remove_file(copy.join(".git")).unwrap();
    git(&copy, &["init", "--quiet"]);
    let program = copy.join("hook.sh");
    let hooks =
        ["pre-commit", "pre-push", "post-commit"].map(|hook| copy.join(".git/hooks").join(hook));
    for path in hooks.iter().chain([&program]) {
        std::fs::write(path, &script).unwrap();
        Command::new("chmod").arg("+x").arg(path).status().unwrap();
    }
    git(
        &copy,
        &["config", "core.fsmonitor", program.to_str().unwrap()],
    );

    assert_eq!(chat_copy::changes(&chat).await.0, [file("hook.sh", 2, 0)]);
    let author = ("Ada Dorn", "ada@example.com");
    let pushed = chat_copy::push(&chat, &repository, "weacon/hooks", "A hook", author);
    pushed.await.unwrap();
    assert!(!ran.exists());
    // The same git, left to find the folder by itself, would have run them.
    git(&copy, &["status"]);
    assert!(ran.exists());
}
