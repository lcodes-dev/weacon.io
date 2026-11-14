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

use crate::rules::{PLANNED, RULES};

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
            out.extend(sources::check_sources(rule, &read_sources(dir)?));
        }
    }
    Ok(out)
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

fn read_sources(dir: &Path) -> Result<Vec<SourceFile>, String> {
    let mut sources = Vec::new();
    let mut pending: Vec<PathBuf> = ["build.rs", "src", "tests", "benches"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.exists())
        .collect();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries =
                fs::read_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            for entry in entries {
                pending.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text = read(&path)?;
            let path = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
            sources.push(SourceFile { path, text });
        }
    }
    Ok(sources)
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

    #[test]
    fn a_folder_without_a_workspace_is_an_error() {
        violations(&std::env::temp_dir()).unwrap_err();
    }
}
