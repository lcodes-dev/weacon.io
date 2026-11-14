//! The Claude Code plugin the core gives its runs: the files in `integrations/claude`, with this
//! build's version and this program's path filled in.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{code, to_hex};
use weacon_version::Version;

const MANIFEST: &str = include_str!("../../../integrations/claude/.claude-plugin/plugin.json");
const HOOKS: &str = include_str!("../../../integrations/claude/hooks/hooks.json");
const SKILL: &str = include_str!("../../../integrations/claude/skills/weacon-map/SKILL.md");

const COMMAND: &str = "/hooks/SessionStart/0/hooks/0/command";

/// The plugin each run is given, checked before every launch.
#[derive(Clone, Debug)]
pub(crate) struct Plugin {
    dir: PathBuf,
    files: [(&'static str, String); 3],
    log: Logger,
}

impl Plugin {
    /// This program's plugin under `state_dir`, or none, logged, when it can't be made.
    pub(crate) fn new(state_dir: &Path, version: &Version, log: &Logger) -> Option<Self> {
        let plugin = std::env::current_exe()
            .map_err(|error| failed().because(error))
            .and_then(|program| Self::at(state_dir, &program, version, log));
        logged(log, plugin)
    }

    /// The plugin for `program`, which its hook runs.
    pub(crate) fn at(
        state_dir: &Path,
        program: &Path,
        version: &Version,
        log: &Logger,
    ) -> Result<Self, Fault> {
        let files = render(program, version)?;
        let dir = state_dir
            .join("integrations")
            .join("claude")
            .join(name(&files));
        Ok(Self {
            dir,
            files,
            log: log.clone(),
        })
    }

    /// The plugin's folder, rewritten when anything in it differs; none, logged, when it
    /// can't be.
    pub(crate) fn folder(&self) -> Option<PathBuf> {
        logged(
            &self.log,
            self.ensure().map_err(|error| failed().because(error)),
        )
    }

    // A folder an agent changed is set aside, since every later run would load it.
    fn ensure(&self) -> io::Result<PathBuf> {
        if !holds(&self.dir, &self.files) {
            if fs::symlink_metadata(&self.dir).is_ok() {
                fs::rename(&self.dir, aside(&self.dir, "stale"))?;
            }
            write(&self.dir, &self.files)?;
        }
        Ok(self.dir.clone())
    }
}

fn logged<T>(log: &Logger, made: Result<T, Fault>) -> Option<T> {
    made.inspect_err(|fault| log.warn("plugin failed", &[("err", Field::error(fault))]))
        .ok()
}

fn render(program: &Path, version: &Version) -> Result<[(&'static str, String); 3], Fault> {
    let program = program.to_str().ok_or_else(failed)?;
    let mut manifest: Value = serde_json::from_str(MANIFEST).map_err(|_| failed())?;
    *manifest.get_mut("version").ok_or_else(failed)? = version.version.into();
    let mut hooks: Value = serde_json::from_str(HOOKS).map_err(|_| failed())?;
    *hooks.pointer_mut(COMMAND).ok_or_else(failed)? =
        format!("{} hook session-start", quoted(program)).into();
    Ok([
        (".claude-plugin/plugin.json", manifest.to_string()),
        ("hooks/hooks.json", hooks.to_string()),
        ("skills/weacon-map/SKILL.md", SKILL.to_owned()),
    ])
}

// Claude Code runs a hook's command in a POSIX shell, on Windows too.
fn quoted(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// A folder name that changes whenever any file does.
fn name(files: &[(&str, String)]) -> String {
    let mut hash = Sha256::new();
    for (path, text) in files {
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(text.as_bytes());
        hash.update([0]);
    }
    to_hex(hash.finalize().get(..8).unwrap_or_default())
}

/// Whether `dir` holds exactly `files`, as plain files.
fn holds(dir: &Path, files: &[(&str, String)]) -> bool {
    let mut found = Vec::new();
    if listed(dir, dir, &mut found).is_err() {
        return false;
    }
    found.sort();
    let mut expected: Vec<PathBuf> = files.iter().map(|(path, _)| PathBuf::from(path)).collect();
    expected.sort();
    found == expected
        && files
            .iter()
            .all(|(path, text)| fs::read_to_string(dir.join(path)).is_ok_and(|held| held == *text))
}

fn listed(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            listed(root, &path, found)?;
        } else if kind.is_file() {
            found.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        } else {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    Ok(())
}

// Written beside the folder, then renamed, so a folder that exists is always whole.
fn write(dir: &Path, files: &[(&str, String)]) -> io::Result<()> {
    let staging = aside(dir, "part");
    let written = fill(&staging, files).and_then(|()| fs::rename(&staging, dir));
    if written.is_err() {
        drop(fs::remove_dir_all(&staging));
    }
    written
}

fn fill(staging: &Path, files: &[(&str, String)]) -> io::Result<()> {
    for (path, text) in files {
        let file = staging.join(path);
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(&file, text)?;
        // Read-only, so an agent's edit can't change what every later run loads.
        let mut permissions = fs::metadata(&file)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions)?;
    }
    Ok(())
}

fn aside(dir: &Path, what: &str) -> PathBuf {
    dir.with_extension(format!("{what}-{:x}", weacon_appkit::random_id()))
}

fn failed() -> Fault {
    Fault::new(
        code!("plugin.write_failed"),
        "The agents' plugin couldn't be written",
    )
}

#[cfg(test)]
mod tests {
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::test_support::{VERSION, messages};

    fn json(dir: &Path, path: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.join(path)).unwrap()).unwrap()
    }

    fn plugin(state: &Path, program: &str, version: &Version) -> Plugin {
        Plugin::at(state, Path::new(program), version, &recording_logger().0).unwrap()
    }

    #[test]
    fn the_plugin_names_this_build_and_runs_this_program() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;

        let dir = plugin(state, "/opt/it's here/weacon", &VERSION)
            .folder()
            .unwrap();

        assert_eq!(
            dir.parent(),
            Some(state.join("integrations/claude").as_path())
        );
        assert_eq!(json(&dir, ".claude-plugin/plugin.json")["version"], "0.1.0");
        let hooks = json(&dir, "hooks/hooks.json");
        let command = hooks.pointer(COMMAND).unwrap();
        assert_eq!(command, r"'/opt/it'\''s here/weacon' hook session-start");
        let skill = fs::read_to_string(dir.join("skills/weacon-map/SKILL.md")).unwrap();
        assert_eq!(skill, SKILL);
        let manifest = dir.join(".claude-plugin/plugin.json");
        assert!(fs::metadata(manifest).unwrap().permissions().readonly());
    }

    #[test]
    fn each_build_and_program_has_its_own_folder() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;
        let newer = Version {
            version: "0.2.0",
            ..VERSION
        };

        let first = plugin(state, "/opt/weacon", &VERSION).folder().unwrap();
        let again = plugin(state, "/opt/weacon", &VERSION).folder().unwrap();
        let moved = plugin(state, "/usr/bin/weacon", &VERSION).folder().unwrap();
        let updated = plugin(state, "/opt/weacon", &newer).folder().unwrap();

        assert_eq!(first, again);
        assert_ne!(first, moved);
        assert_ne!(first, updated);
        let folders = fs::read_dir(state.join("integrations/claude")).unwrap();
        assert_eq!(folders.count(), 3);
    }

    #[test]
    fn a_folder_that_was_changed_is_set_aside_and_written_again() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;
        let plugin = plugin(state, "/opt/weacon", &VERSION);
        let dir = plugin.folder().unwrap();
        let hooks = dir.join("hooks/hooks.json");

        fs::remove_file(&hooks).unwrap();
        assert_eq!(plugin.folder().unwrap(), dir);
        fs::write(dir.join("hooks/more.json"), "{}").unwrap();
        assert_eq!(plugin.folder().unwrap(), dir);

        assert!(!dir.join("hooks/more.json").exists());
        assert_eq!(
            json(&dir, "hooks/hooks.json").pointer(COMMAND).unwrap(),
            "'/opt/weacon' hook session-start"
        );
        let folders = fs::read_dir(state.join("integrations/claude")).unwrap();
        assert_eq!(folders.count(), 3);
    }

    #[test]
    fn a_plugin_that_cant_be_written_is_none_and_logged() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;
        fs::write(state.join("integrations"), "").unwrap();
        let (log, records) = recording_logger();

        let plugin = Plugin::new(state, &VERSION, &log).unwrap();

        assert_eq!(plugin.folder(), None);
        assert_eq!(messages(&records), ["plugin failed"]);
        assert_eq!(fs::read_dir(state).unwrap().count(), 1);
        let elsewhere = Plugin::new(&state.join("elsewhere"), &VERSION, &log).unwrap();
        assert!(
            elsewhere
                .folder()
                .unwrap()
                .join("hooks/hooks.json")
                .is_file()
        );
    }
}
