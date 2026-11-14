//! The person's own terminal program, opened in a folder on their desktop: fixed arguments,
//! the folder as the only thing that varies, and no shell between.

use std::path::Path;
use std::process::Command;

use super::Env;

/// The command that opens this desktop's terminal in `folder`; none for a folder that isn't
/// absolute, or where there is no desktop or no way to open one without a shell.
pub(crate) fn terminal_in(folder: &Path, env: Env<'_>) -> Option<Command> {
    if !folder.is_absolute() {
        return None;
    }
    let mut command = desktop_terminal(folder, env)?;
    command.current_dir(folder);
    Some(command)
}

// Terminal.app opens a window in a folder given as a path; it runs nothing it's given.
#[cfg(target_os = "macos")]
fn desktop_terminal(folder: &Path, _: Env<'_>) -> Option<Command> {
    Path::new("/System/Applications/Utilities/Terminal.app")
        .is_dir()
        .then(|| {
            let mut command = Command::new("/usr/bin/open");
            command.args(["-a", "Terminal"]).arg(folder);
            command
        })
}

// Only through the desktop's chosen terminal, and only in a desktop session.
#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_terminal(folder: &Path, env: Env<'_>) -> Option<Command> {
    super::var(env, "WAYLAND_DISPLAY").or_else(|| super::var(env, "DISPLAY"))?;
    let program = super::find_program("xdg-terminal-exec", &super::var(env, "PATH")?)?;
    let mut dir = std::ffi::OsString::from("--dir=");
    dir.push(folder);
    let mut command = Command::new(program);
    command.arg(dir);
    Some(command)
}

// The console's own shell in a console window of its own.
#[cfg(windows)]
fn desktop_terminal(_: &Path, env: Env<'_>) -> Option<Command> {
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
        assert!(terminal_in(Path::new("chats/c-1"), &env(&[])).is_none());
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
        let command = terminal_in(&folder, &desktop).unwrap();
        assert_eq!(command.get_program(), program.as_os_str());
        let args: Vec<_> = command.get_args().collect();
        let dir_arg = format!("--dir={}", folder.display());
        assert_eq!(args, [OsStr::new(&dir_arg)]);
        assert_eq!(command.get_current_dir(), Some(folder.as_path()));

        let x11 = env(&[("PATH", path), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &x11).is_some());
        let no_desktop = env(&[("PATH", path)]);
        assert!(terminal_in(&folder, &no_desktop).is_none());
        let no_terminal = env(&[("PATH", "/nowhere"), ("DISPLAY", ":0")]);
        assert!(terminal_in(&folder, &no_terminal).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_opens_the_console_shell_in_the_folder() {
        let folder = Path::new(r"C:\Users\ada\chat");
        let command = terminal_in(folder, &env(&[("ComSpec", r"C:\Windows\System32\cmd.exe")]));
        let command = command.unwrap();
        assert_eq!(
            command.get_program(),
            OsStr::new(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_current_dir(), Some(folder));
        assert!(terminal_in(folder, &env(&[])).is_none());
    }
}
