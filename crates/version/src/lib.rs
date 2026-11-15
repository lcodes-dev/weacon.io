//! The version and commit each Weacon program reports.
//!
//! An app calls [`emit`] from its `build.rs` and reads its own [`Version`] with [`version!`].

use std::env;
use std::fmt;
use std::path::Path;
use std::process::Command;

const VARIABLE: &str = "WEACON_COMMIT";
const UNKNOWN: &str = "unknown";

/// A program's package name, version and the commit it was built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub name: &'static str,
    pub version: &'static str,
    pub commit: &'static str,
}

/// The calling package's [`Version`]. It compiles only when the package's build script calls
/// [`emit`].
#[macro_export]
macro_rules! version {
    () => {
        $crate::Version {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            commit: env!("WEACON_COMMIT"),
        }
    };
}

impl Version {
    /// The release that error reports carry: `weacon-core@0.1.0+<commit>`.
    #[must_use]
    pub fn release(&self) -> String {
        format!("{}@{}+{}", self.name, self.version, self.commit)
    }

    /// `0.1.0 (<commit>)`: what `--version` prints after the name.
    #[must_use]
    pub fn with_commit(&self) -> String {
        format!("{} ({})", self.version, self.commit)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.name, self.with_commit())
    }
}

/// Tells Cargo the commit being built: `WEACON_COMMIT` when not blank, else `git`; `unknown`
/// when that isn't a commit hash. Call it from `build.rs`.
#[expect(
    clippy::print_stdout,
    reason = "Cargo reads build-script instructions from stdout"
)]
pub fn emit() {
    let dir = env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default();
    let given = env::var(VARIABLE).ok();
    for line in instructions(Path::new(&dir), given.as_deref()) {
        println!("{line}");
    }
}

fn instructions(dir: &Path, given: Option<&str>) -> Vec<String> {
    let (commit, watched) = match given.map(str::trim).filter(|commit| !commit.is_empty()) {
        Some(commit) => (commit.to_owned(), Vec::new()),
        None => from_git(dir).unwrap_or_default(),
    };
    let commit = if is_commit(&commit) {
        commit.as_str()
    } else {
        UNKNOWN
    };
    let mut lines = vec![format!("cargo::rerun-if-env-changed={VARIABLE}")];
    lines.extend(
        watched
            .iter()
            .map(|path| format!("cargo::rerun-if-changed={path}")),
    );
    lines.push(format!("cargo::rustc-env={VARIABLE}={commit}"));
    lines
}

// Only paths that exist are watched: Cargo reruns the script on every build for a missing one.
fn from_git(dir: &Path) -> Option<(String, Vec<String>)> {
    let output = git_command()
        .args([
            "rev-parse",
            "HEAD",
            "--git-path",
            "HEAD",
            "--git-common-dir",
        ])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let [commit, head, common] = text.lines().collect::<Vec<_>>().try_into().ok()?;
    let watched = [
        head.to_owned(),
        format!("{common}/refs/heads"),
        format!("{common}/packed-refs"),
        format!("{common}/reftable"),
    ]
    .into_iter()
    .filter(|path| dir.join(path).exists())
    .collect();
    Some((commit.to_owned(), watched))
}

// `git` for the folder it runs in, even when a hook has pointed git at another repository.
fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

fn is_commit(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn version() -> Version {
        Version {
            name: "weacon-core",
            version: "0.1.0",
            commit: SHA,
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = git_command().args(args).current_dir(dir).output().unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn shows_name_version_and_commit() {
        assert_eq!(version().to_string(), format!("weacon-core 0.1.0 ({SHA})"));
        assert_eq!(version().release(), format!("weacon-core@0.1.0+{SHA}"));
        assert_eq!(version().with_commit(), format!("0.1.0 ({SHA})"));
    }

    #[test]
    fn takes_the_commit_from_git_and_watches_head_and_refs() {
        let dir = TempDir::new().unwrap();
        git(&dir, &["init", "--quiet"]);
        git(
            &dir,
            &[
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--allow-empty",
                "--message=first",
            ],
        );
        let commit = git(&dir, &["rev-parse", "HEAD"]);

        let lines = instructions(&dir, None);

        assert_eq!(
            lines,
            [
                "cargo::rerun-if-env-changed=WEACON_COMMIT".to_owned(),
                "cargo::rerun-if-changed=.git/HEAD".to_owned(),
                "cargo::rerun-if-changed=.git/refs/heads".to_owned(),
                format!("cargo::rustc-env=WEACON_COMMIT={commit}"),
            ]
        );
    }

    #[test]
    fn prefers_a_given_commit_and_rejects_anything_else() {
        let dir = Path::new("/nonexistent");

        assert_eq!(
            instructions(dir, Some(" 1a2b3c4\n")).last().unwrap(),
            "cargo::rustc-env=WEACON_COMMIT=1a2b3c4"
        );
        for given in ["1a2b3c", "not-a-sha", "1a2b3c4\ncargo::rustc-env=X=1"] {
            assert_eq!(
                instructions(dir, Some(given)).last().unwrap(),
                "cargo::rustc-env=WEACON_COMMIT=unknown",
                "{given:?}"
            );
        }
    }

    #[test]
    fn reports_unknown_outside_a_git_checkout() {
        let dir = TempDir::new().unwrap();

        assert_eq!(instructions(&dir, Some(" ")), instructions(&dir, None));
        assert_eq!(
            instructions(&dir, None),
            [
                "cargo::rerun-if-env-changed=WEACON_COMMIT",
                "cargo::rustc-env=WEACON_COMMIT=unknown"
            ]
        );
    }
}
