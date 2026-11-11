//! The driver for Codex, through its app server.

mod wire;

use tokio::process::Command;

pub(crate) use wire::{Wire, answer};

use crate::launch::Launch;

// Sign-ins that would win over the workspace's own; removed, never read.
const OTHER_SIGN_INS: [&str; 2] = ["CODEX_API_KEY", "OPENAI_API_KEY"];

/// `codex` as an app server.
pub(crate) fn command(launch: &Launch) -> Command {
    let mut command = with_workspace_sign_in(launch);
    command.arg("app-server").current_dir(&launch.cwd);
    command
}

/// `codex`'s own check of its sign-in, which exits with success when signed in; it runs
/// wherever Weacon does.
pub(crate) fn status(launch: &Launch) -> Command {
    let mut command = with_workspace_sign_in(launch);
    command.args(["login", "status"]);
    command
}

fn with_workspace_sign_in(launch: &Launch) -> Command {
    let mut command = Command::new(&launch.program);
    for name in OTHER_SIGN_INS {
        command.env_remove(name);
    }
    command.env("CODEX_HOME", &launch.config_dir);
    command
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use weacon_protocol::AgentKind;

    use super::*;

    #[test]
    fn both_commands_run_with_the_workspaces_config_and_no_other_sign_in() {
        let launch = Launch {
            agent: AgentKind::Codex,
            program: "/work/codex".into(),
            config_dir: "/work/config".into(),
            cwd: "/work".into(),
        };
        let status_command = status(&launch);
        let status_args: Vec<_> = status_command.as_std().get_args().collect();

        assert_eq!(status_args, ["login", "status"]);
        assert_eq!(
            command(&launch).as_std().get_args().collect::<Vec<_>>(),
            ["app-server"]
        );
        for command in [command(&launch), status(&launch)] {
            let command = command.as_std();
            let envs: Vec<_> = command.get_envs().collect();

            assert!(envs.contains(&(OsStr::new("CODEX_HOME"), Some(OsStr::new("/work/config")))));
            for name in OTHER_SIGN_INS {
                assert!(envs.contains(&(OsStr::new(name), None)), "{name}");
            }
        }
        let run = command(&launch);
        assert_eq!(run.as_std().get_current_dir(), Some(Path::new("/work")));
        assert_eq!(status(&launch).as_std().get_current_dir(), None);
    }
}
