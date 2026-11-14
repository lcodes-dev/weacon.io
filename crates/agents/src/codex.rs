//! The driver for Codex, through its app server.

pub(crate) mod transcript;
mod wire;

use tokio::process::Command;

pub(crate) use wire::{Wire, answer};

use crate::launch::{Launch, SignInEnv};

// Its config folder's variable, and the sign-ins and providers that would win over it.
/// The folders and files under home that hold its sign-in.
pub(crate) const CREDENTIALS: [&str; 1] = [".codex"];

const SIGN_IN_ENV: SignInEnv = SignInEnv {
    config_var: "CODEX_HOME",
    prefixes: &["CODEX_", "OPENAI_"],
    names: &[],
};

/// `codex` as an app server.
pub(crate) fn command(launch: &Launch) -> Command {
    let mut command = SIGN_IN_ENV.command(launch);
    command.arg("app-server").current_dir(&launch.cwd);
    command
}

/// `codex`'s own check of its sign-in, which exits with success when signed in; it runs
/// wherever Weacon does.
pub(crate) fn status(launch: &Launch) -> Command {
    let mut command = SIGN_IN_ENV.command(launch);
    command.args(["login", "status"]);
    command
}

/// `codex`'s own sign-in, for the person's terminal.
pub(crate) fn login(launch: &Launch) -> Command {
    let mut command = SIGN_IN_ENV.command(launch);
    command.arg("login");
    command
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use weacon_protocol::AgentKind;

    use super::*;

    #[test]
    fn every_command_runs_with_the_workspaces_config() {
        let launch = crate::test_support::launch(AgentKind::Codex, Path::new("/work"));
        let status_command = status(&launch);
        let status_args: Vec<_> = status_command.as_std().get_args().collect();

        assert_eq!(status_args, ["login", "status"]);
        assert_eq!(
            command(&launch).as_std().get_args().collect::<Vec<_>>(),
            ["app-server"]
        );
        let login = login(&launch);
        assert_eq!(login.as_std().get_args().collect::<Vec<_>>(), ["login"]);
        for command in [command(&launch), status(&launch), login] {
            let command = command.as_std();
            let config = (
                OsStr::new("CODEX_HOME"),
                Some(launch.config_dir.as_os_str()),
            );
            assert!(command.get_envs().any(|env| env == config));
        }
        let run = command(&launch);
        assert_eq!(run.as_std().get_current_dir(), Some(Path::new("/work")));
        assert_eq!(status(&launch).as_std().get_current_dir(), None);
    }

    #[test]
    fn other_sign_ins_and_providers_are_known_and_the_rest_is_kept() {
        for name in ["CODEX_API_KEY", "OPENAI_API_KEY", "OPENAI_BASE_URL"] {
            assert!(SIGN_IN_ENV.matches(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "ANTHROPIC_API_KEY"] {
            assert!(!SIGN_IN_ENV.matches(OsStr::new(name)), "{name}");
        }
    }
}
