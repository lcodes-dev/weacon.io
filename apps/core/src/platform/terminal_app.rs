//! The person's own terminal program, opened in a folder on their desktop: fixed arguments,
//! the folder and the core's own command as the only things that vary, and no shell between.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use super::Env;

/// The command that opens this desktop's terminal in `folder`, running `run` in it where the
/// terminal takes a command; none for a folder or a program that isn't absolute, or where
/// there is no desktop or no way to open one without a shell.
pub(crate) fn terminal_in(folder: &Path, env: Env<'_>, run: &[OsString]) -> Option<Command> {
    let absolute = |path: &OsString| Path::new(path).is_absolute();
    if !folder.is_absolute() || run.first().is_some_and(|program| !absolute(program)) {
        return None;
    }
    let mut command = desktop_terminal(folder, env, run)?;
    command.current_dir(folder);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

// Terminal.app opens a window in a folder given as a path; it runs nothing it's given.
#[cfg(target_os = "macos")]
fn desktop_terminal(folder: &Path, _: Env<'_>, _: &[OsString]) -> Option<Command> {
    Path::new("/System/Applications/Utilities/Terminal.app")
        .is_dir()
        .then(|| {
            let mut command = Command::new("/usr/bin/open");
            command.args(["-a", "Terminal"]).arg(folder);
            command
        })
}

// Only through the desktop's chosen terminal, and only in a desktop session. The program is
// absolute, so the terminal never reads it as an option of its own.
#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_terminal(folder: &Path, env: Env<'_>, run: &[OsString]) -> Option<Command> {
    super::var(env, "WAYLAND_DISPLAY").or_else(|| super::var(env, "DISPLAY"))?;
    let program = super::find_program("xdg-terminal-exec", &super::var(env, "PATH")?)?;
    let mut dir = OsString::from("--dir=");
    dir.push(folder);
    let mut command = Command::new(program);
    command.arg(dir).args(run);
    Some(command)
}

// The console's own shell in a console window of its own.
#[cfg(windows)]
fn desktop_terminal(_: &Path, env: Env<'_>, _: &[OsString]) -> Option<Command> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let shell = super::var(env, "ComSpec")?;
    let mut command = Command::new(shell);
    command.creation_flags(CREATE_NEW_CONSOLE);
    Some(command)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::env;

    #[test]
    fn a_folder_that_isnt_absolute_opens_nothing() {
        assert!(terminal_in(Path::new("chats/c-1"), &env(&[]), &[]).is_none());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
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

        let x11 = env(&[("PATH", path), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &x11, &[]).is_some());
        let no_desktop = env(&[("PATH", path)]);
        assert!(terminal_in(&folder, &no_desktop, &[]).is_none());
        let no_terminal = env(&[("PATH", "/nowhere"), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &no_terminal, &[]).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_opens_the_console_shell_in_the_folder() {
        let folder = Path::new(r"C:\Users\ada\chat");
        let shell = env(&[("ComSpec", r"C:\Windows\System32\cmd.exe")]);
        let command = terminal_in(folder, &shell, &[r"C:\weacon.exe".into()]);
        let command = command.unwrap();
        assert_eq!(
            command.get_program(),
            OsStr::new(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_current_dir(), Some(folder));
        assert!(terminal_in(folder, &env(&[]), &[]).is_none());
    }
}
