//! The driver for Codex, through its app server.

pub(crate) mod transcript;
mod wire;

use tokio::process::Command;
use weacon_protocol::{AgentSession, App};

pub(crate) use wire::{Wire, answer};

use crate::launch::{Launch, OtherSignIns};
use crate::vendor::Vendor;
use crate::wire::Wire as RunWire;

pub(crate) const VENDOR: Vendor = Vendor {
    config_variable: "CODEX_HOME",
    other_sign_ins: OtherSignIns {
        prefixes: &["CODEX_", "OPENAI_"],
        names: &[],
    },
    status: &["login", "status"],
    login: &["login"],
    // Codex prints nothing that says where its sign-in comes from, so its exit decides alone.
    own_sign_in: |_| true,
    takes_mcp: false,
    drive,
    resume: None,
};

/// `codex` as an app server, its wire resuming `session` when given.
fn drive(launch: &Launch, app: &App, session: Option<&AgentSession>) -> (Command, RunWire) {
    let mut command = launch.with_workspace_sign_in(&VENDOR);
    command.arg("app-server").current_dir(&launch.cwd);
    let wire = Wire::new(session.cloned(), app, launch.access);
    (command, RunWire::Codex(wire))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use weacon_protocol::AgentKind;

    use super::*;
    use crate::test_support::{app, launch};

    fn args(command: &Command) -> Vec<&OsStr> {
        command.as_std().get_args().collect()
    }

    #[test]
    fn every_command_runs_with_the_workspaces_config() {
        let launch = launch(AgentKind::Codex, "/work/codex", Path::new("/work"));
        let status = launch.status_command(&VENDOR);
        let login = launch.login_command(&VENDOR);
        let (run, _) = drive(&launch, &app(), None);

        assert_eq!(args(&status), ["login", "status"]);
        assert_eq!(args(&login), ["login"]);
        assert_eq!(args(&run), ["app-server"]);
        for (name, command) in [("run", &run), ("status", &status), ("login", &login)] {
            let config = (
                OsStr::new("CODEX_HOME"),
                Some(launch.config_dir.as_os_str()),
            );
            assert!(
                command.as_std().get_envs().any(|env| env == config),
                "{name}"
            );
        }
        assert_eq!(run.as_std().get_current_dir(), Some(Path::new("/work")));
        assert_eq!(status.as_std().get_current_dir(), None);
    }

    #[test]
    fn other_sign_ins_and_providers_are_known_and_the_rest_is_kept() {
        for name in ["CODEX_API_KEY", "OPENAI_API_KEY", "OPENAI_BASE_URL"] {
            assert!(VENDOR.other_sign_ins.matches(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "ANTHROPIC_API_KEY"] {
            assert!(!VENDOR.other_sign_ins.matches(OsStr::new(name)), "{name}");
        }
    }
}
