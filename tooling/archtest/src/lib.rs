//! Checks that the workspace keeps its dependency rules and conventions; see `rules.rs`.

mod manifest;
mod rules;
mod sources;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml::Table;

use crate::rules::{INTEGRATIONS_DATA, INTEGRATIONS_FOLDER, PLANNED, RULES};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
pub(crate) struct Package {
    pub(crate) name: String,
    pub(crate) manifest_path: PathBuf,
    pub(crate) dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    kind: Option<Kind>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    #[default]
    Normal,
    Dev,
    Build,
}

impl Kind {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Dev => "dev",
            Self::Build => "build",
        }
    }
}

impl Dependency {
    pub(crate) fn kind(&self) -> Kind {
        self.kind.unwrap_or_default()
    }
}

/// A source file, with its path inside its package.
pub(crate) struct SourceFile {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
}

/// Every way the workspace at `root` breaks its rules, one sentence each.
///
/// # Errors
/// When `cargo metadata` fails or a manifest or source file can't be read.
pub fn violations(root: &Path) -> Result<Vec<String>, String> {
    let metadata = read_metadata(root)?;
    let names: BTreeSet<&str> = metadata
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let mut out = manifest::check_rules(RULES, PLANNED, &names);
    for package in &metadata.packages {
        let dir = package
            .manifest_path
            .parent()
            .ok_or("a manifest path has no folder")?;
        let manifest = read_manifest(&package.manifest_path)?;
        out.extend(manifest::check_manifest(&package.name, &manifest));
        if let Some(rule) = RULES.iter().find(|rule| rule.package == package.name) {
            let is_app = dir
                .strip_prefix(&metadata.workspace_root)
                .is_ok_and(|path| path.starts_with("apps"));
            out.extend(manifest::check_dependencies(package, rule, &names));
            if is_app {
                out.extend(manifest::check_app_needs(package));
            }
            let (sources, links) = read_sources(dir)?;
            out.extend(sources::check_sources(rule, &sources));
            out.extend(links.iter().map(|link| {
                let (name, link) = (&package.name, link.display());
                format!("{name}: {link} is a link; archtest checks only a package's own files")
            }));
        }
    }
    out.extend(check_integrations(&metadata.workspace_root)?);
    Ok(out)
}

/// Each link in the integrations folder, and each file that isn't one of the data kinds it may
/// hold.
fn check_integrations(root: &Path) -> Result<Vec<String>, String> {
    let mut found: Vec<String> = files_under(&root.join(INTEGRATIONS_FOLDER))?
        .iter()
        .filter_map(|path| {
            let link = is_link(path);
            let kind = path.extension().and_then(|extension| extension.to_str());
            let data = kind.is_some_and(|kind| INTEGRATIONS_DATA.contains(&kind));
            let shown = path.strip_prefix(root).unwrap_or(path).display();
            if link {
                Some(format!(
                    "{shown} is a link; {INTEGRATIONS_FOLDER}/ holds only its own files"
                ))
            } else if data {
                None
            } else {
                Some(format!(
                    "{shown} isn't data; {INTEGRATIONS_FOLDER}/ holds only JSON and Markdown"
                ))
            }
        })
        .collect();
    found.sort();
    Ok(found)
}

fn read_metadata(root: &Path) -> Result<Metadata, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--offline",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cargo metadata didn't start: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata's output is unexpected: {error}"))
}

fn read_manifest(path: &Path) -> Result<Table, String> {
    let text = read(path)?;
    toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// A package's Rust files, and the links among them, which are never followed.
fn read_sources(dir: &Path) -> Result<(Vec<SourceFile>, Vec<PathBuf>), String> {
    let mut sources = Vec::new();
    let mut links = Vec::new();
    for name in ["build.rs", "src", "tests", "benches"] {
        for path in files_under(&dir.join(name))? {
            let relative = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
            if is_link(&path) {
                links.push(relative);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = read(&path)?;
                sources.push(SourceFile {
                    path: relative,
                    text,
                });
            }
        }
    }
    links.sort();
    Ok((sources, links))
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink())
}

/// Every file and link at or under `path`, without following links; none when nothing is there.
fn files_under(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(kind) = fs::symlink_metadata(&path).map(|metadata| metadata.file_type()) else {
            continue;
        };
        if kind.is_dir() {
            let entries =
                fs::read_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            for entry in entries {
                pending.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_keeps_its_rules() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let found = violations(&root).unwrap();

        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    // archtest may use nothing in the repo, so not weacon_testing's TempDir.
    fn temp_dir() -> PathBuf {
        use std::hash::{BuildHasher, RandomState};

        let id = RandomState::new().hash_one(std::process::id());
        let dir = std::env::temp_dir().join(format!("weacon-archtest-{id:016x}"));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_without_a_workspace_is_an_error() {
        let dir = temp_dir();
        let found = violations(&dir);
        fs::remove_dir(&dir).unwrap();

        found.unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn links_among_a_packages_sources_are_found_not_followed() {
        let dir = temp_dir();
        let src = dir.join("src");
        fs::create_dir_all(src.join("real")).unwrap();
        fs::write(src.join("lib.rs"), "fn f() {}").unwrap();
        fs::write(src.join("real/inner.rs"), "fn g() {}").unwrap();
        std::os::unix::fs::symlink("real", src.join("folder")).unwrap();
        std::os::unix::fs::symlink("lib.rs", src.join("file.rs")).unwrap();

        let read = read_sources(&dir);
        fs::remove_dir_all(&dir).unwrap();

        let (sources, links) = read.unwrap();
        let mut paths: Vec<_> = sources.iter().map(|file| file.path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                PathBuf::from("src/lib.rs"),
                PathBuf::from("src/real/inner.rs")
            ]
        );
        assert_eq!(
            links,
            [PathBuf::from("src/file.rs"), PathBuf::from("src/folder")]
        );
    }

    #[test]
    fn the_integrations_folder_holds_only_data_and_may_be_missing() {
        let root = temp_dir();
        let hooks = root.join("integrations/claude/hooks");
        fs::create_dir_all(&hooks).unwrap();
        for name in ["hooks.json", "run.sh", "notes"] {
            fs::write(hooks.join(name), "").unwrap();
        }
        fs::write(root.join("integrations/claude/SKILL.md"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("..", hooks.join("up.json")).unwrap();

        let found = check_integrations(&root);
        let missing = check_integrations(&root.join("nowhere"));
        fs::remove_dir_all(&root).unwrap();

        let expected = |name: &str| {
            let path = Path::new("integrations/claude/hooks").join(name);
            let path = path.display();
            format!("{path} isn't data; integrations/ holds only JSON and Markdown")
        };
        let mut expected = vec![expected("notes"), expected("run.sh")];
        if cfg!(unix) {
            let link = Path::new("integrations/claude/hooks/up.json");
            let link = link.display();
            expected.push(format!(
                "{link} is a link; integrations/ holds only its own files"
            ));
        }
        expected.sort();
        assert_eq!(found.unwrap(), expected);
        assert!(missing.unwrap().is_empty());
    }
}
