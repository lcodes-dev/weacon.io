//! A Linux desktop's chosen terminal, through `xdg-terminal-exec`, which runs the command it's
//! given as a program and its arguments, with no shell.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::platform::{Env, find_program, var};

/// Only in a desktop session. The program in `run` is absolute, so the terminal never reads it
/// as an option of its own.
pub(super) fn desktop_terminal(folder: &Path, env: Env<'_>, run: &[OsString]) -> Option<Command> {
    var(env, "WAYLAND_DISPLAY").or_else(|| var(env, "DISPLAY"))?;
    let path = var(env, "PATH")?;
    let terminal = find_program("xdg-terminal-exec", &path)?;
    let mut dir = OsString::from("--dir=");
    dir.push(folder);
    // Under a systemd service, a scope of its own keeps the window open when the service restarts.
    let scope = var(env, "INVOCATION_ID").and_then(|_| find_program("systemd-run", &path));
    let mut command = match scope {
        Some(systemd_run) => {
            let mut command = Command::new(systemd_run);
            command.args(["--user", "--scope", "--collect", "--quiet", "--"]);
            command.arg(terminal);
            command
        }
        None => Command::new(terminal),
    };
    command.arg(dir).args(run);
    Some(super::detached(command))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use weacon_testing::TempDir;

    use super::super::terminal_in;
    use super::*;
    use crate::test_support::env;

    #[test]
    fn a_linux_desktop_opens_its_chosen_terminal_in_the_folder() {
        let dir = TempDir::new().unwrap();
        let program = crate::platform::plant_program(&dir.join("bin"), "xdg-terminal-exec", 0o755);
        let path = dir.join("bin");
        let path = path.to_str().unwrap();
        let folder = dir.join("chat");
        let desktop = env(&[("PATH", path), ("WAYLAND_DISPLAY", "wayland-0")]);
        let run = ["/opt/weacon", "resume", "--chat=c-1"].map(OsString::from);
        let command = terminal_in(&folder, &desktop, &run).unwrap();
        assert_eq!(command.get_program(), program.as_os_str());
        let dir_arg = format!("--dir={}", folder.display());
        let expected =
            std::iter::once(OsStr::new(&dir_arg)).chain(run.iter().map(OsString::as_os_str));
        assert!(command.get_args().eq(expected));
        assert_eq!(command.get_current_dir(), Some(folder.as_path()));

        let relative = ["weacon", "resume"].map(OsString::from);
        assert!(terminal_in(&folder, &desktop, &relative).is_none());
        assert!(terminal_in(Path::new("chat"), &desktop, &run).is_none());

        let x11 = env(&[("PATH", path), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &x11, &[]).is_some());
        let no_desktop = env(&[("PATH", path)]);
        assert!(terminal_in(&folder, &no_desktop, &[]).is_none());
        let no_terminal = env(&[("PATH", "/nowhere"), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &no_terminal, &[]).is_none());
        let no_scope = env(&[("PATH", path), ("DISPLAY", ":0"), ("INVOCATION_ID", "1f")]);
        let direct = terminal_in(&folder, &no_scope, &[]).unwrap();
        assert_eq!(direct.get_program(), program.as_os_str());
    }

    #[test]
    fn under_a_service_the_terminal_gets_a_scope_of_its_own() {
        let dir = TempDir::new().unwrap();
        let bin = dir.join("bin");
        let terminal = crate::platform::plant_program(&bin, "xdg-terminal-exec", 0o755);
        let systemd_run = crate::platform::plant_program(&bin, "systemd-run", 0o755);
        let path = bin.to_str().unwrap();
        let folder = dir.join("chat");
        let run = ["/opt/weacon", "resume"].map(OsString::from);
        let service = env(&[("PATH", path), ("DISPLAY", ":0"), ("INVOCATION_ID", "1f")]);

        let command = terminal_in(&folder, &service, &run).unwrap();

        assert_eq!(command.get_program(), systemd_run.as_os_str());
        let dir_arg = OsString::from(format!("--dir={}", folder.display()));
        let scope = ["--user", "--scope", "--collect", "--quiet", "--"].map(OsString::from);
        let expected: Vec<OsString> = scope
            .into_iter()
            .chain([terminal.into_os_string(), dir_arg])
            .chain(run)
            .collect();
        assert!(
            command
                .get_args()
                .eq(expected.iter().map(OsString::as_os_str))
        );
        let desktop = env(&[("PATH", path), ("DISPLAY", ":0")]);
        let direct = terminal_in(&folder, &desktop, &[]).unwrap();
        assert_ne!(direct.get_program(), systemd_run.as_os_str());
    }
}
