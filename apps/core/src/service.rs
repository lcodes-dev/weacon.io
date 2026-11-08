//! `weacon service`: the daemon as a systemd user service that keeps running after logout.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use weacon_observability::{Fault, Logger};
use weacon_protocol::code;

use crate::app::{App, state_unavailable};
use crate::cli::ServiceCommand;
use crate::platform::{self, Env};

const UNIT: &str = "weacon.service";

/// Runs `program` with `args` and says whether it succeeded; tests pass a fake.
pub(crate) type Runner<'a> = &'a mut dyn FnMut(&str, &[&str]) -> io::Result<bool>;

/// A command, and what failed when it does.
struct Step {
    program: &'static str,
    args: &'static [&'static str],
    failed: &'static str,
}

// Linger keeps the user's services running with nobody logged in, and starts them at boot.
const LINGER: Step = Step {
    program: "loginctl",
    args: &["enable-linger"],
    failed: "Lingering couldn't be turned on for this user",
};

const RELOAD: Step = Step {
    program: "systemctl",
    args: &["--user", "daemon-reload"],
    failed: "systemd couldn't reload its units",
};

const ENABLE: Step = Step {
    program: "systemctl",
    args: &["--user", "enable", UNIT],
    failed: "The service couldn't be enabled",
};

// A restart, not a start, so a reinstall runs the new binary.
const RESTART: Step = Step {
    program: "systemctl",
    args: &["--user", "restart", UNIT],
    failed: "The service couldn't start",
};

const STOP: Step = Step {
    program: "systemctl",
    args: &["--user", "disable", "--now", UNIT],
    failed: "The service couldn't be stopped",
};

/// Installs or removes the service for this binary.
pub(crate) fn run(
    app: &App,
    command: ServiceCommand,
    env: Env<'_>,
    runner: Runner<'_>,
) -> Result<(), Fault> {
    let exe = std::env::current_exe().map_err(|error| {
        Fault::new(
            code!("service.no_exe"),
            "The weacon binary couldn't be found",
        )
        .because(error)
    })?;
    apply(command, env, &exe, &app.log, runner)
}

fn apply(
    command: ServiceCommand,
    env: Env<'_>,
    exe: &Path,
    log: &Logger,
    runner: Runner<'_>,
) -> Result<(), Fault> {
    if !platform::USER_SERVICES {
        return Err(Fault::new(
            code!("service.unsupported"),
            "Services are only installed on Linux with systemd",
        ));
    }
    let path = unit_path(env).ok_or_else(|| {
        Fault::new(
            code!("service.no_config_dir"),
            "The service has nowhere to go: set HOME",
        )
    })?;
    match command {
        ServiceCommand::Install => {
            let state_dir = platform::state_dir(env).ok_or_else(state_unavailable)?;
            platform::write_whole(&path, unit(exe, &state_dir)?.as_bytes())
                .map_err(write_failed)?;
            [LINGER, RELOAD, ENABLE, RESTART]
                .iter()
                .try_for_each(|step| run_step(step, runner))?;
            log.info("service installed", &[]);
        }
        ServiceCommand::Uninstall => {
            if path.exists() {
                run_step(&STOP, runner)?;
                fs::remove_file(&path).map_err(write_failed)?;
                run_step(&RELOAD, runner)?;
            }
            log.info("service removed", &[]);
        }
    }
    Ok(())
}

fn run_step(step: &Step, runner: Runner<'_>) -> Result<(), Fault> {
    let failed = || Fault::new(code!("service.step_failed"), step.failed);
    match runner(step.program, step.args) {
        Ok(true) => Ok(()),
        Ok(false) => Err(failed()),
        Err(error) => Err(failed().because(error)),
    }
}

/// Where systemd looks for this user's units.
fn unit_path(env: Env<'_>) -> Option<PathBuf> {
    platform::xdg_dir(env, "XDG_CONFIG_HOME", ".config")
        .map(|config| config.join("systemd/user").join(UNIT))
}

/// The unit for the daemon at `exe`, pinned to `state_dir` so it and this user's commands
/// use the same one.
fn unit(exe: &Path, state_dir: &Path) -> Result<String, Fault> {
    let bad_path = || {
        Fault::new(
            code!("service.bad_path"),
            "The binary's or the state folder's path can't go in a systemd unit",
        )
    };
    let exe = exe.to_str().and_then(quoted).ok_or_else(bad_path)?;
    let state = state_dir
        .to_str()
        .and_then(|dir| quoted(&format!("WEACON_STATE_DIR={dir}")))
        .ok_or_else(bad_path)?;
    Ok(format!(
        "[Unit]\n\
         Description=Weacon core\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Environment={state}\n\
         ExecStart={exe} daemon\n\
         Restart=on-failure\n\
         RestartSec=5s\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    ))
}

/// `text` as a quoted systemd word, or `None` when it has a `$` or a control character.
fn quoted(text: &str) -> Option<String> {
    let mut word = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' | '"' => word.extend(['\\', character]),
            // Specifiers expand even inside quotes.
            '%' => word.push_str("%%"),
            // Expanded differently in each setting, so refused rather than escaped.
            '$' => return None,
            _ if character.is_control() => return None,
            _ => word.push(character),
        }
    }
    word.push('"');
    Some(word)
}

fn write_failed(error: io::Error) -> Fault {
    Fault::new(
        code!("service.write_failed"),
        "The service's unit file couldn't be written",
    )
    .because(error)
}

/// Runs a command with its output dropped and its errors shown.
pub(crate) fn system(program: &str, args: &[&str]) -> io::Result<bool> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Records, env, recording_logger};

    type Calls = Vec<String>;

    // Records each call and fails the one named `failing`.
    fn runner<'a>(
        calls: &'a mut Calls,
        failing: &'a str,
    ) -> impl FnMut(&str, &[&str]) -> io::Result<bool> + 'a {
        move |program, args| {
            let call = format!("{program} {}", args.join(" "));
            let ok = call != failing;
            calls.push(call);
            Ok(ok)
        }
    }

    #[test]
    fn the_unit_runs_the_daemon_at_its_quoted_path() {
        let text = unit(
            Path::new("/home/ada/.local/bin/weacon"),
            Path::new("/home/ada/.local/state/weacon"),
        )
        .unwrap();

        assert!(
            text.contains("\nEnvironment=\"WEACON_STATE_DIR=/home/ada/.local/state/weacon\"\n")
        );
        assert!(text.contains("\nExecStart=\"/home/ada/.local/bin/weacon\" daemon\n"));
        assert!(text.contains("\nRestart=on-failure\n"));
        assert!(text.ends_with("\n[Install]\nWantedBy=default.target\n"));
        assert_eq!(
            quoted("/opt/my apps/w\\e\"a%con").as_deref(),
            Some("\"/opt/my apps/w\\\\e\\\"a%%con\"")
        );
        assert_eq!(quoted("/opt/we\ncon"), None);
        assert_eq!(quoted("/opt/$HOME"), None);
        let error = unit(Path::new("/usr/bin/weacon"), Path::new("/srv/$state")).unwrap_err();
        assert_eq!(error.code().as_str(), "service.bad_path");
    }

    #[cfg(unix)]
    #[test]
    fn system_runs_a_program_and_says_whether_it_succeeded() {
        assert!(system("true", &[]).unwrap());
        assert!(!system("sh", &["-c", "exit 3"]).unwrap());
        system("weacon-no-such-program", &[]).unwrap_err();
    }

    const EXE: &str = "/usr/bin/weacon";

    fn act(
        action: ServiceCommand,
        vars: &[(&str, &str)],
        runner: Runner<'_>,
    ) -> (Result<(), Fault>, Records) {
        let (log, records) = recording_logger();
        let result = apply(action, &env(vars), Path::new(EXE), &log, runner);
        (result, records)
    }

    #[cfg(target_os = "linux")]
    mod on_linux {
        use weacon_testing::TempDir;

        use super::*;
        use crate::test_support::messages;

        #[test]
        fn the_unit_goes_in_the_users_systemd_folder() {
            let path = |vars: &[(&str, &str)]| unit_path(&env(vars));
            let home = ("HOME", "/home/ada");

            assert_eq!(
                path(&[home]),
                Some(PathBuf::from(
                    "/home/ada/.config/systemd/user/weacon.service"
                ))
            );
            assert_eq!(
                path(&[home, ("XDG_CONFIG_HOME", "/data/config")]),
                Some(PathBuf::from("/data/config/systemd/user/weacon.service"))
            );
            assert_eq!(path(&[home, ("XDG_CONFIG_HOME", "config")]), path(&[home]));
            assert_eq!(path(&[]), None);
        }

        const INSTALLED: [&str; 4] = [
            "loginctl enable-linger",
            "systemctl --user daemon-reload",
            "systemctl --user enable weacon.service",
            "systemctl --user restart weacon.service",
        ];

        fn vars(dir: &Path) -> [(&str, &str); 2] {
            [
                ("XDG_CONFIG_HOME", dir.to_str().unwrap()),
                ("XDG_STATE_HOME", dir.to_str().unwrap()),
            ]
        }

        fn in_dir(
            action: ServiceCommand,
            dir: &Path,
            calls: &mut Calls,
            failing: &str,
        ) -> (Result<(), Fault>, Records) {
            act(action, &vars(dir), &mut runner(calls, failing))
        }

        #[test]
        fn install_writes_the_unit_then_lingers_enables_and_restarts() {
            let dir = TempDir::new().unwrap();
            let mut calls = Calls::new();

            let (result, records) = in_dir(ServiceCommand::Install, &dir, &mut calls, "");

            result.unwrap();
            let file = dir.join("systemd/user/weacon.service");
            assert_eq!(
                fs::read_to_string(&file).unwrap(),
                unit(Path::new(EXE), &dir.join("weacon")).unwrap()
            );
            assert!(!file.with_extension("new").exists());
            assert_eq!(calls, INSTALLED);
            assert_eq!(messages(&records), ["service installed"]);
        }

        #[test]
        fn a_failed_step_stops_the_install_and_says_which() {
            let dir = TempDir::new().unwrap();
            let mut calls = Calls::new();

            let error = in_dir(ServiceCommand::Install, &dir, &mut calls, INSTALLED[2])
                .0
                .unwrap_err();

            assert_eq!(error.code().as_str(), "service.step_failed");
            assert_eq!(error.to_string(), "The service couldn't be enabled");
            assert_eq!(calls, INSTALLED[..3]);
            let mut missing = |_: &str, _: &[&str]| Err(io::ErrorKind::NotFound.into());
            let gone = act(ServiceCommand::Install, &vars(&dir), &mut missing)
                .0
                .unwrap_err();
            assert_eq!(
                gone.to_string(),
                "Lingering couldn't be turned on for this user"
            );
            assert!(std::error::Error::source(&gone).is_some());
        }

        #[test]
        fn uninstall_stops_and_removes_an_installed_service_only() {
            let dir = TempDir::new().unwrap();
            let mut calls = Calls::new();
            in_dir(ServiceCommand::Install, &dir, &mut calls, "")
                .0
                .unwrap();
            calls.clear();

            let (result, records) = in_dir(ServiceCommand::Uninstall, &dir, &mut calls, "");
            result.unwrap();
            in_dir(ServiceCommand::Uninstall, &dir, &mut calls, "")
                .0
                .unwrap();

            assert_eq!(
                calls,
                [
                    "systemctl --user disable --now weacon.service",
                    "systemctl --user daemon-reload"
                ]
            );
            assert!(!dir.join("systemd/user/weacon.service").exists());
            assert_eq!(messages(&records), ["service removed"]);
        }

        #[test]
        fn without_a_home_there_is_nowhere_to_install() {
            let mut calls = Calls::new();
            let error = act(ServiceCommand::Install, &[], &mut runner(&mut calls, ""))
                .0
                .unwrap_err();

            assert_eq!(error.code().as_str(), "service.no_config_dir");
            let no_state = [("XDG_CONFIG_HOME", "/srv/config")];
            let error = act(
                ServiceCommand::Install,
                &no_state,
                &mut runner(&mut calls, ""),
            )
            .0
            .unwrap_err();
            assert_eq!(error.code().as_str(), "state.unavailable");
            assert!(calls.is_empty());
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn elsewhere_there_is_no_service() {
        let mut calls = Calls::new();
        let error = act(
            ServiceCommand::Install,
            &[("HOME", "/home/ada")],
            &mut runner(&mut calls, ""),
        )
        .0
        .unwrap_err();

        assert_eq!(error.code().as_str(), "service.unsupported");
        assert!(calls.is_empty());
    }
}
