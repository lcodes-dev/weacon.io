//! What a chat's agents changed in its worktree since its branch began, committed or not.

use std::fs;
use std::io::Read as _;
use std::path::Path;

use weacon_observability::Fault;
use weacon_protocol::{DiffContent, DiffFile, DiffLine, FileChange, Hunk, LineOp, MAX_FRAME, code};

use super::{Git, run, text};

// A file's changes past this many bytes of patch are only said to be too large, so each still
// fits one frame with room for its JSON.
const MAX_PATCH: usize = MAX_FRAME / 4;

impl Git {
    /// Every file changed in `worktree` since the commit `base`, untracked ones included, in
    /// the order of their paths.
    pub(crate) fn changes(&self, worktree: &Path, base: &str) -> Result<Vec<DiffFile>, Fault> {
        if !is_commit_id(base) {
            return Err(Fault::new(
                code!("git.not_a_commit"),
                "That isn't a commit id",
            ));
        }
        let listed = text(run(self.command(worktree).args([
            "diff",
            "--name-status",
            "-z",
            "-M",
            "--no-ext-diff",
            base,
            "--",
        ]))?)?;
        let untracked = text(run(self.command(worktree).args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ]))?)?;
        let mut files = Vec::new();
        for (change, path) in name_status(&listed) {
            let content = self.patch(worktree, base, &change, &path)?;
            files.push(DiffFile {
                path,
                change,
                content,
            });
        }
        for path in untracked.split('\0').filter(|path| !path.is_empty()) {
            files.push(DiffFile {
                path: path.to_owned(),
                change: FileChange::Added,
                content: added(&worktree.join(path)),
            });
        }
        files.sort_by(|one, other| one.path.cmp(&other.path));
        Ok(files)
    }

    fn patch(
        &self,
        worktree: &Path,
        base: &str,
        change: &FileChange,
        path: &str,
    ) -> Result<DiffContent, Fault> {
        let mut command = self.command(worktree);
        command.args([
            "diff",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            base,
            "--",
        ]);
        if let FileChange::Renamed { from } = change {
            command.arg(from);
        }
        let out = run(command.arg(path))?;
        if out.len() > MAX_PATCH {
            return Ok(DiffContent::TooLarge);
        }
        Ok(String::from_utf8(out).map_or(DiffContent::Binary, |patch| content(&patch)))
    }
}

// A full SHA-1 or SHA-256 commit id, in lowercase hex.
fn is_commit_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64)
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

// The changes `git diff --name-status -z -M` lists; a type change counts as a change.
fn name_status(listed: &str) -> Vec<(FileChange, String)> {
    let mut fields = listed.split('\0').filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        let change = match status.chars().next() {
            Some('A') => FileChange::Added,
            Some('D') => FileChange::Removed,
            Some('R') => match fields.next() {
                Some(from) => FileChange::Renamed {
                    from: from.to_owned(),
                },
                None => break,
            },
            _ => FileChange::Modified,
        };
        let Some(path) = fields.next() else { break };
        changes.push((change, path.to_owned()));
    }
    changes
}

// One file's patch as hunks; git's own note on a binary file makes it binary.
fn content(patch: &str) -> DiffContent {
    if patch.lines().any(|line| line.starts_with("Binary files ")) {
        return DiffContent::Binary;
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in patch.split('\n') {
        if let Some(header) = line.strip_prefix("@@ ") {
            if let Some(hunk) = hunk_start(header) {
                hunks.push(hunk);
            }
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let op = match line.chars().next() {
            Some(' ') => LineOp::Context,
            Some('+') => LineOp::Added,
            Some('-') => LineOp::Removed,
            Some('\\') => {
                if let Some(last) = hunk.lines.last_mut() {
                    last.no_newline = true;
                }
                continue;
            }
            _ => continue,
        };
        hunk.lines.push(DiffLine {
            op,
            text: line.get(1..).unwrap_or_default().to_owned(),
            no_newline: false,
        });
    }
    DiffContent::Text { hunks }
}

// `-a,b +c,d @@ …` as an empty hunk starting at `a` and `c`.
fn hunk_start(header: &str) -> Option<Hunk> {
    let mut sides = header.split(' ');
    let start = |side: Option<&str>, sign: char| {
        side?
            .strip_prefix(sign)?
            .split(',')
            .next()?
            .parse::<u32>()
            .ok()
    };
    let old_start = start(sides.next(), '-')?;
    let new_start = start(sides.next(), '+')?;
    Some(Hunk {
        old_start,
        new_start,
        lines: Vec::new(),
    })
}

// An untracked file, read only when it's a plain file: a link's target never is.
fn added(file: &Path) -> DiffContent {
    let Ok(meta) = file.symlink_metadata() else {
        return DiffContent::Binary;
    };
    if !meta.is_file() {
        return DiffContent::Binary;
    }
    if usize::try_from(meta.len()).map_or(true, |len| len > MAX_PATCH) {
        return DiffContent::TooLarge;
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_PATCH).unwrap_or(u64::MAX);
    let read = fs::File::open(file).and_then(|opened| opened.take(limit).read_to_end(&mut bytes));
    match String::from_utf8(bytes) {
        Ok(text) if read.is_ok() && !text.contains('\0') => DiffContent::Text {
            hunks: added_hunks(&text),
        },
        _ => DiffContent::Binary,
    }
}

// A new file's text as one hunk of added lines; an empty file has none.
fn added_hunks(text: &str) -> Vec<Hunk> {
    if text.is_empty() {
        return Vec::new();
    }
    let ends_in_newline = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines: Vec<DiffLine> = body
        .split('\n')
        .map(|line| DiffLine {
            op: LineOp::Added,
            text: line.to_owned(),
            no_newline: false,
        })
        .collect();
    if let Some(last) = lines.last_mut() {
        last.no_newline = !ends_in_newline;
    }
    vec![Hunk {
        old_start: 0,
        new_start: 1,
        lines,
    }]
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::git::tests::{chat, fixture, git, repo};

    // A chat's worktree, the commit it began at, and its git.
    fn begun(dir: &Path) -> (Git, std::path::PathBuf, String) {
        let (git, repo) = (git(dir), repo(dir));
        fs::write(repo.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::write(repo.join("old.txt"), "moved\nas it is\n").unwrap();
        fs::write(repo.join("gone.txt"), "bye\n").unwrap();
        fixture(dir, &repo, &["add", "lib.rs", "old.txt", "gone.txt"]);
        fixture(dir, &repo, &["commit", "--quiet", "-m", "More"]);
        let path = dir.join("trees/c-d");
        git.chat_worktree(&repo, &chat("d"), &path).unwrap();
        let base = text(run(git.command(&path).args(["rev-parse", "HEAD"])).unwrap()).unwrap();
        (git, path, base.trim_end().to_owned())
    }

    fn line(op: LineOp, text: &str) -> DiffLine {
        DiffLine {
            op,
            text: text.to_owned(),
            no_newline: false,
        }
    }

    #[test]
    fn an_untouched_worktree_has_no_changes() {
        let dir = TempDir::new().unwrap();
        let (git, path, base) = begun(&dir);

        assert_eq!(git.changes(&path, &base).unwrap(), []);
    }

    // A worktree where an agent changed, renamed, removed and added files, some committed.
    fn agent_changes(dir: &Path) -> Vec<DiffFile> {
        let (git, path, base) = begun(dir);
        fs::write(path.join("lib.rs"), "fn a() {}\nfn c() {}").unwrap();
        fs::rename(path.join("old.txt"), path.join("new.txt")).unwrap();
        fs::remove_file(path.join("gone.txt")).unwrap();
        fixture(
            dir,
            &path,
            &["add", "-A", "--", "old.txt", "new.txt", "gone.txt"],
        );
        fixture(dir, &path, &["commit", "--quiet", "-m", "Agent"]);
        fs::write(path.join("notes[1].md"), "# Notes\n").unwrap();
        fs::write(path.join("data.bin"), [0, 1, 2]).unwrap();

        git.changes(&path, &base).unwrap()
    }

    #[test]
    fn every_kind_of_change_is_listed_by_path_committed_or_not() {
        let dir = TempDir::new().unwrap();
        let files = agent_changes(&dir);

        let summary: Vec<(&str, &FileChange)> = files
            .iter()
            .map(|file| (file.path.as_str(), &file.change))
            .collect();
        let renamed = FileChange::Renamed {
            from: "old.txt".to_owned(),
        };
        assert_eq!(
            summary,
            [
                ("data.bin", &FileChange::Added),
                ("gone.txt", &FileChange::Removed),
                ("lib.rs", &FileChange::Modified),
                ("new.txt", &renamed),
                ("notes[1].md", &FileChange::Added),
            ]
        );
    }

    #[test]
    fn each_change_carries_its_lines() {
        let dir = TempDir::new().unwrap();
        let files = agent_changes(&dir);

        let lib = files.iter().find(|file| file.path == "lib.rs").unwrap();
        let ends = DiffLine {
            no_newline: true,
            ..line(LineOp::Added, "fn c() {}")
        };
        assert_eq!(
            lib.content,
            DiffContent::Text {
                hunks: vec![Hunk {
                    old_start: 1,
                    new_start: 1,
                    lines: vec![
                        line(LineOp::Context, "fn a() {}"),
                        line(LineOp::Removed, "fn b() {}"),
                        ends,
                    ],
                }],
            }
        );
        let notes = files
            .iter()
            .find(|file| file.path == "notes[1].md")
            .unwrap();
        assert_eq!(
            notes.content,
            DiffContent::Text {
                hunks: vec![Hunk {
                    old_start: 0,
                    new_start: 1,
                    lines: vec![line(LineOp::Added, "# Notes")],
                }],
            }
        );
        let data = files.iter().find(|file| file.path == "data.bin").unwrap();
        assert_eq!(data.content, DiffContent::Binary);
        let moved = files.iter().find(|file| file.path == "new.txt").unwrap();
        assert_eq!(moved.content, DiffContent::Text { hunks: Vec::new() });
    }

    #[test]
    fn a_path_that_looks_like_a_pattern_is_only_a_path() {
        let dir = TempDir::new().unwrap();
        let (git, path, _) = begun(&dir);
        // As a pattern, `[ab]` would also match `a`.
        for name in ["[ab]", "a"] {
            fs::write(path.join(name), "old\n").unwrap();
        }
        fixture(&dir, &path, &["add", "[ab]", "a"]);
        fixture(&dir, &path, &["commit", "--quiet", "-m", "Pair"]);
        let base = text(run(git.command(&path).args(["rev-parse", "HEAD"])).unwrap()).unwrap();
        fs::write(path.join("[ab]"), "new\n").unwrap();
        fs::write(path.join("a"), "other\n").unwrap();

        let files = git.changes(&path, base.trim_end()).unwrap();

        let pair = files.iter().find(|file| file.path == "[ab]").unwrap();
        let lines = vec![line(LineOp::Removed, "old"), line(LineOp::Added, "new")];
        let hunks = vec![Hunk {
            old_start: 1,
            new_start: 1,
            lines,
        }];
        assert_eq!(pair.content, DiffContent::Text { hunks });
    }

    #[test]
    fn an_untracked_link_is_never_followed() {
        let dir = TempDir::new().unwrap();
        let (git, path, base) = begun(&dir);
        fs::write(dir.join("secret"), "token\n").unwrap();
        if !crate::platform::plant_link(&dir.join("secret"), &path.join("key")) {
            return;
        }

        let files = git.changes(&path, &base).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, DiffContent::Binary);
    }

    #[test]
    fn a_file_too_large_for_a_frame_says_so() {
        let dir = TempDir::new().unwrap();
        let (git, path, base) = begun(&dir);
        let big = "line\n".repeat(MAX_PATCH / 4);
        fs::write(path.join("lib.rs"), &big).unwrap();
        fs::write(path.join("big.txt"), &big).unwrap();

        let files = git.changes(&path, &base).unwrap();

        assert!(
            files
                .iter()
                .all(|file| file.content == DiffContent::TooLarge),
            "{files:?}"
        );
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn only_a_full_commit_id_is_a_base() {
        let dir = TempDir::new().unwrap();
        let (git, path, _) = begun(&dir);

        for base in ["HEAD", "--output=x", "abc", &"A".repeat(40)] {
            let error = git.changes(&path, base).unwrap_err();
            assert_eq!(error.code().as_str(), "git.not_a_commit", "{base}");
        }
        assert!(is_commit_id(&"a1".repeat(20)) && is_commit_id(&"f".repeat(64)));
    }

    #[test]
    fn a_listing_cut_short_ends_where_it_does() {
        let listed = "M\0a.rs\0R087\0b.rs\0c.rs\0T\0d\0D\0e\0A";
        let changes = name_status(listed);
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[3], (FileChange::Removed, "e".to_owned()));
        assert_eq!(name_status("R100\0only"), []);
    }

    #[test]
    fn hunks_need_their_headers_and_binary_notes_win() {
        assert_eq!(
            content("noise\n+lost\n@@ -3,2 +4 @@ fn x\n-a\n\\ No newline at end of file\n"),
            DiffContent::Text {
                hunks: vec![Hunk {
                    old_start: 3,
                    new_start: 4,
                    lines: vec![DiffLine {
                        no_newline: true,
                        ..line(LineOp::Removed, "a")
                    }],
                }],
            }
        );
        assert_eq!(
            content("Binary files a/x and b/x differ\n"),
            DiffContent::Binary
        );
        assert_eq!(
            content("@@ nonsense @@\n+a\n"),
            DiffContent::Text { hunks: Vec::new() }
        );
        assert_eq!(added_hunks(""), []);
    }
}
