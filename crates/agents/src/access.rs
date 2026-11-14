//! The prompts a workspace's access level answers without asking a person.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use weacon_protocol::AccessLevel;

use crate::platform;

/// What a run may do: its access level, the folder it works in, and the workspace's own
/// folder, which holds the agent's settings.
#[derive(Clone)]
pub(crate) struct Rules {
    pub(crate) level: AccessLevel,
    pub(crate) cwd: PathBuf,
    pub(crate) workspace: PathBuf,
}

/// How a prompt is settled.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Ask,
    Refuse,
    /// Allowed if the file `may_change`, else asked.
    Check(PathBuf),
}

/// A prompt about changing `file`, waiting for the file to be checked.
pub(crate) struct Check {
    pub(crate) request: Value,
    pub(crate) tool: String,
    pub(crate) input: Value,
    pub(crate) file: PathBuf,
}

impl Rules {
    /// Rules that ask a person about everything.
    #[cfg(test)]
    pub(crate) fn ask_first() -> Self {
        Self {
            level: AccessLevel::AskFirst,
            cwd: PathBuf::new(),
            workspace: PathBuf::new(),
        }
    }

    /// The decision for a prompt about changing `file`, or about anything else when `None`.
    pub(crate) fn decide(&self, file: Option<PathBuf>) -> Decision {
        match self.level {
            AccessLevel::AskFirst => Decision::Ask,
            AccessLevel::Full => file.map_or(Decision::Ask, Decision::Check),
            AccessLevel::Off | AccessLevel::ReadOnly | AccessLevel::Newer => Decision::Refuse,
        }
    }

    /// Whether `file` is an absolute path inside the working folder as written and once
    /// symlinks are resolved, both times through no hidden name, outside the workspace's own
    /// folder, and not a file with another name. A relative path is never allowed: the agent
    /// may have changed folder. Blocks on the file system, but only for a path that passes as
    /// written.
    pub(crate) fn may_change(&self, file: &Path) -> bool {
        // Checked before any lookup, so a path elsewhere, such as a network share, is never
        // opened.
        if !file.is_absolute() || !plainly_inside(file, &self.cwd) {
            return false;
        }
        let (Ok(root), Some(file)) = (self.cwd.canonicalize(), resolve(file)) else {
            return false;
        };
        let in_workspace = self
            .workspace
            .canonicalize()
            .is_ok_and(|workspace| file.starts_with(workspace));
        !in_workspace && plainly_inside(&file, &root) && has_one_name(&file)
    }
}

// `path` with symlinks resolved as far as it exists; `None` if the rest climbs with `..` or
// anything on the way can't be resolved, such as a symlink to nowhere.
pub(crate) fn resolve(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut base = path;
    loop {
        if let Ok(found) = base.canonicalize() {
            return Some(
                missing
                    .iter()
                    .rev()
                    .fold(found, |path, name| path.join(name)),
            );
        }
        match base.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return None,
        }
        missing.push(base.file_name()?);
        base = base.parent()?;
    }
}

// Whether `file` is below `folder`, through plain names only.
fn plainly_inside(file: &Path, folder: &Path) -> bool {
    file.strip_prefix(folder).is_ok_and(|inside| {
        inside.components().next().is_some()
            && inside
                .components()
                .all(|part| matches!(part, Component::Normal(name) if is_plain(name)))
    })
}

// A file another name also reaches, a hard link, may be one outside the folder.
fn has_one_name(file: &Path) -> bool {
    match platform::links(file) {
        Ok(links) => links == 1,
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

// Not hidden, as config such as `.git` and `.claude` is, and no stream: `:` opens one on
// Windows.
fn is_plain(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| !name.starts_with('.') && !name.contains(':'))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    fn rules(level: AccessLevel, cwd: &Path) -> Rules {
        Rules {
            level,
            cwd: cwd.to_path_buf(),
            workspace: cwd.join("state/workspaces/w-w1"),
        }
    }

    #[test]
    fn each_level_decides_only_what_it_should() {
        let rules = |level| rules(level, Path::new("/work"));
        let file = || Some(PathBuf::from("/work/README.md"));

        for refused in [AccessLevel::Off, AccessLevel::ReadOnly, AccessLevel::Newer] {
            assert_eq!(rules(refused).decide(file()), Decision::Refuse);
            assert_eq!(rules(refused).decide(None), Decision::Refuse);
        }
        assert_eq!(rules(AccessLevel::AskFirst).decide(file()), Decision::Ask);
        assert_eq!(rules(AccessLevel::AskFirst).decide(None), Decision::Ask);
        assert_eq!(
            rules(AccessLevel::Full).decide(file()),
            Decision::Check(PathBuf::from("/work/README.md"))
        );
        assert_eq!(rules(AccessLevel::Full).decide(None), Decision::Ask);
        assert_eq!(Rules::ask_first().decide(file()), Decision::Ask);
    }

    #[test]
    fn only_absolute_files_inside_the_folder_and_outside_its_config_may_change() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.join("work");
        fs::create_dir_all(cwd.join("src")).unwrap();
        fs::create_dir_all(cwd.join(".git")).unwrap();
        fs::create_dir_all(cwd.join("state/workspaces/w-w1/claude")).unwrap();
        fs::write(cwd.join("README.md"), "").unwrap();
        let rules = rules(AccessLevel::Full, &cwd);
        let may = |file: &str| rules.may_change(&cwd.join(file));

        for allowed in [
            "README.md",
            "src/new.rs",
            "src/new/deeper/file.rs",
            "state/other.md",
        ] {
            assert!(may(allowed), "{allowed}");
        }
        for refused in [
            "../outside.md",
            "src/../../outside.md",
            "missing/../../outside.md",
            ".git/hooks/pre-commit",
            ".Git/config",
            ".git./config",
            ".git /config",
            "notes.md:stream",
            ".claude/settings.json",
            "src/.codex/config.toml",
            "vendor/.git/config",
            "state/workspaces/w-w1/claude/settings.json",
            "state/workspaces/w-w1/workspace.json",
            "",
        ] {
            assert!(!may(refused), "{refused}");
        }
        // A file in the way can't be resolved; Windows may report it as missing instead.
        if cfg!(unix) {
            assert!(!may("README.md/inside.md"));
        }
        for relative in ["README.md", "~/.bashrc", "./README.md", "~"] {
            assert!(!rules.may_change(Path::new(relative)), "{relative}");
        }
        assert!(!rules.may_change(&dir.join("outside.md")));
        assert!(!rules.may_change(Path::new(r"\\host\share\file.md")));
        let missing = Rules {
            cwd: dir.join("missing"),
            ..rules
        };
        assert!(!missing.may_change(&dir.join("missing/README.md")));
    }

    #[test]
    fn hidden_paths_and_files_with_another_name_are_asked_about() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join("single.md"), "").unwrap();
        fs::write(dir.join("elsewhere.md"), "").unwrap();
        fs::hard_link(dir.join("elsewhere.md"), cwd.join("linked.md")).unwrap();
        let rules = rules(AccessLevel::Full, &cwd);
        let may = |file: &str| rules.may_change(&cwd.join(file));

        assert!(may("single.md"));
        for refused in [
            ".envrc",
            ".husky/pre-commit",
            ".vscode/tasks.json",
            "src/.hidden.rs",
            "linked.md",
        ] {
            assert!(!may(refused), "{refused}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_followed_before_the_folder_is_checked() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let cwd = dir.join("work");
        fs::create_dir_all(cwd.join(".git")).unwrap();
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        symlink(dir.join("elsewhere"), cwd.join("out")).unwrap();
        symlink(cwd.join(".git"), cwd.join("hidden")).unwrap();
        symlink(&cwd, dir.join("link")).unwrap();
        symlink(dir.join("missing.md"), cwd.join("dangling.md")).unwrap();
        symlink(cwd.join("plain"), cwd.join(".claude")).unwrap();
        fs::create_dir(cwd.join("plain")).unwrap();
        let rules = rules(AccessLevel::Full, &cwd);
        let may = |file: &str| rules.may_change(&cwd.join(file));

        assert!(!may("out/file.md"));
        assert!(!may("hidden/hooks/pre-commit"));
        assert!(!may("dangling.md"));
        assert!(!may("dangling.md/file.md"));
        assert!(!may(".claude/settings.json"));
        assert!(may("plain/settings.json"));
        // Inside once resolved, but not as written.
        assert!(!rules.may_change(&dir.join("link/file.md")));
        let linked = Rules {
            cwd: dir.join("link"),
            ..rules
        };
        assert!(linked.may_change(&dir.join("link/file.md")));
    }
}
