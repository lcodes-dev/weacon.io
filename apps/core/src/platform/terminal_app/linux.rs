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
    let program = find_program("xdg-terminal-exec", &var(env, "PATH")?)?;
    let mut dir = OsString::from("--dir=");
    dir.push(folder);
    let mut command = Command::new(program);
    command.arg(dir).args(run);
    Some(command)
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

        let x11 = env(&[("PATH", path), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &x11, &[]).is_some());
        let no_desktop = env(&[("PATH", path)]);
        assert!(terminal_in(&folder, &no_desktop, &[]).is_none());
        let no_terminal = env(&[("PATH", "/nowhere"), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &no_terminal, &[]).is_none());
    }
}
