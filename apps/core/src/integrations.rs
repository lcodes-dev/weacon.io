//! What the core gives its runs: the Claude Code plugin in `integrations/claude`, with this
//! build's version and this program's path filled in, and Codex's hooks in `integrations/codex`.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde_json::Value;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::code;

const MANIFEST: &str = include_str!("../../../integrations/claude/.claude-plugin/plugin.json");
const HOOKS: &str = include_str!("../../../integrations/claude/hooks/hooks.json");
const SKILL: &str = include_str!("../../../integrations/claude/skills/weacon-map/SKILL.md");
const CODEX_HOOKS: &str = include_str!("../../../integrations/codex/hooks.json");

const COMMAND: &str = "/hooks/SessionStart/0/hooks/0/command";
// So a program that moved or was removed still prints nothing and succeeds.
const QUIET: &str = "2>/dev/null || true";

/// The plugin each run is given, written when the daemon starts.
#[derive(Clone, Debug)]
pub(crate) struct Plugin {
    dir: PathBuf,
    codex: String,
    log: Logger,
}

impl Plugin {
    /// This program's plugin under `state_dir`, or none, logged, when it can't be written.
    pub(crate) fn new(state_dir: &Path, version: &str, log: &Logger) -> Option<Self> {
        std::env::current_exe()
            .map_err(|error| failed().because(error))
            .and_then(|program| Self::at(state_dir, &program, version, log))
            .inspect_err(|fault| log.warn("plugin failed", &[("err", Field::error(fault))]))
            .ok()
    }

    /// Writes the plugin for `program`, which its hook runs, under `state_dir`.
    pub(crate) fn at(
        state_dir: &Path,
        program: &Path,
        version: &str,
        log: &Logger,
    ) -> Result<Self, Fault> {
        let dir = state_dir.join("claude-plugin");
        for (path, text) in render(program, version)? {
            let file = dir.join(path);
            file.parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::write(&file, text))
                .map_err(|error| failed().because(error))?;
        }
        Ok(Self {
            dir,
            codex: hooks(CODEX_HOOKS, program)?,
            log: log.clone(),
        })
    }

    pub(crate) fn folder(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Gives the Codex config folder `config` the start-of-session hook unless it has a hooks
    /// file already. Codex runs it only once a person has reviewed it there, in Codex's `/hooks`.
    pub(crate) fn give_codex(&self, config: &Path) {
        let written = fs::File::create_new(config.join("hooks.json"))
            .and_then(|mut file| file.write_all(self.codex.as_bytes()));
        if let Err(error) = unless_there(written) {
            let fault = failed().because(error);
            self.log
                .warn("codex hooks failed", &[("err", Field::error(&fault))]);
        }
    }
}

// A file already there is kept, so that is no error.
fn unless_there(written: io::Result<()>) -> io::Result<()> {
    match written {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error),
        _ => Ok(()),
    }
}

fn render(program: &Path, version: &str) -> Result<[(&'static str, String); 3], Fault> {
    let mut manifest: Value = serde_json::from_str(MANIFEST).map_err(|_| failed())?;
    *manifest.get_mut("version").ok_or_else(failed)? = version.into();
    Ok([
        (".claude-plugin/plugin.json", manifest.to_string()),
        ("hooks/hooks.json", hooks(HOOKS, program)?),
        ("skills/weacon-map/SKILL.md", SKILL.to_owned()),
    ])
}

// `file`, a hooks file of either agent, with its session-start command running `program`.
fn hooks(file: &str, program: &Path) -> Result<String, Fault> {
    let program = program.to_str().ok_or_else(failed)?;
    let mut hooks: Value = serde_json::from_str(file).map_err(|_| failed())?;
    *hooks.pointer_mut(COMMAND).ok_or_else(failed)? =
        format!("{} hook session-start {QUIET}", quoted(program)).into();
    Ok(hooks.to_string())
}

// Claude Code runs a hook's command in a POSIX shell, on Windows too.
fn quoted(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
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

    fn plugin(state: &Path, program: &str, version: &str) -> Plugin {
        Plugin::at(state, Path::new(program), version, &recording_logger().0).unwrap()
    }

    #[test]
    fn the_plugin_names_this_build_and_runs_this_program() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;

        let dir = plugin(state, "/opt/it's here/weacon", VERSION.version).folder();

        assert_eq!(dir, state.join("claude-plugin"));
        assert_eq!(json(&dir, ".claude-plugin/plugin.json")["version"], "0.1.0");
        let hooks = json(&dir, "hooks/hooks.json");
        let command = hooks.pointer(COMMAND).unwrap();
        assert_eq!(
            command,
            r"'/opt/it'\''s here/weacon' hook session-start 2>/dev/null || true"
        );
        let skill = fs::read_to_string(dir.join("skills/weacon-map/SKILL.md")).unwrap();
        assert_eq!(skill, SKILL);
    }

    #[test]
    fn only_a_file_already_there_is_no_error() {
        let kind = |kind| unless_there(Err(io::Error::from(kind))).map_err(|error| error.kind());

        assert_eq!(unless_there(Ok(())).map_err(|error| error.kind()), Ok(()));
        assert_eq!(kind(io::ErrorKind::AlreadyExists), Ok(()));
        assert_eq!(
            kind(io::ErrorKind::Unsupported),
            Err(io::ErrorKind::Unsupported)
        );
    }

    #[test]
    fn a_codex_folder_gets_the_hook_once_and_keeps_a_file_already_there() {
        let temp = TempDir::new().unwrap();
        let (log, records) = recording_logger();
        let plugin = Plugin::at(&temp, Path::new("/opt/weacon"), VERSION.version, &log).unwrap();
        let (fresh, own, missing) = (temp.join("fresh"), temp.join("own"), temp.join("missing"));
        fs::create_dir_all(&fresh).unwrap();
        fs::create_dir_all(&own).unwrap();
        fs::write(own.join("hooks.json"), "{}").unwrap();

        plugin.give_codex(&fresh);
        plugin.give_codex(&own);

        let hooks = json(&fresh, "hooks.json");
        assert_eq!(
            hooks.pointer(COMMAND).unwrap(),
            "'/opt/weacon' hook session-start 2>/dev/null || true"
        );
        let shipped: Value = serde_json::from_str(CODEX_HOOKS).unwrap();
        assert_eq!(hooks["description"], shipped["description"]);
        assert_eq!(fs::read_to_string(own.join("hooks.json")).unwrap(), "{}");
        assert!(messages(&records).is_empty());
        plugin.give_codex(&missing);
        assert_eq!(messages(&records), ["codex hooks failed"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_is_gone_prints_nothing_and_succeeds() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;
        let gone = state.join("gone/weacon");
        let dir = plugin(state, gone.to_str().unwrap(), VERSION.version).folder();
        let hooks = json(&dir, "hooks/hooks.json");
        let command = hooks.pointer(COMMAND).and_then(Value::as_str).unwrap();

        let ran = std::process::Command::new("sh")
            .args(["-c", command])
            .output()
            .unwrap();

        assert!(ran.status.success());
        assert!(ran.stdout.is_empty() && ran.stderr.is_empty(), "{ran:?}");
    }

    #[test]
    fn a_plugin_that_cant_be_written_is_none_and_logged() {
        let temp = TempDir::new().unwrap();
        let state: &Path = &temp;
        fs::write(state.join("claude-plugin"), "").unwrap();
        let (log, records) = recording_logger();

        assert!(Plugin::new(state, VERSION.version, &log).is_none());

        assert_eq!(messages(&records), ["plugin failed"]);
        let elsewhere = Plugin::new(&state.join("elsewhere"), VERSION.version, &log).unwrap();
        assert!(elsewhere.folder().join("hooks/hooks.json").is_file());
    }
}
