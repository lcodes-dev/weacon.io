//! The driver for Codex, through its app server.

mod wire;

use tokio::process::Command;

pub(crate) use wire::{Wire, answer};

use crate::launch::Launch;

// Sign-ins that would win over the workspace's own; removed, never read.
const OTHER_SIGN_INS: [&str; 2] = ["CODEX_API_KEY", "OPENAI_API_KEY"];

/// `codex` as an app server.
pub(crate) fn command(launch: &Launch) -> Command {
    let mut command = Command::new(&launch.program);
    command.arg("app-server");
    for name in OTHER_SIGN_INS {
        command.env_remove(name);
    }
    command
        .env("CODEX_HOME", &launch.config_dir)
        .current_dir(&launch.cwd);
    command
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use weacon_protocol::AgentKind;

    use super::*;

    #[test]
    fn the_app_server_runs_with_the_workspaces_config_and_no_other_sign_in() {
        let command = command(&Launch {
            agent: AgentKind::Codex,
            program: "/work/codex".into(),
            config_dir: "/work/config".into(),
            cwd: "/work".into(),
        });
        let command = command.as_std();
        let envs: Vec<_> = command.get_envs().collect();

        assert_eq!(command.get_args().collect::<Vec<_>>(), ["app-server"]);
        assert!(envs.contains(&(OsStr::new("CODEX_HOME"), Some(OsStr::new("/work/config")))));
        for name in OTHER_SIGN_INS {
            assert!(envs.contains(&(OsStr::new(name), None)), "{name}");
        }
        assert_eq!(command.get_current_dir(), Some(Path::new("/work")));
    }
}
