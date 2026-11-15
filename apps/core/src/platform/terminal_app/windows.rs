//! A console window of its own, running the console's own shell with no arguments, so nothing
//! the core is given reaches it.

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::platform::{Env, var};

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

pub(super) fn desktop_terminal(_: &Path, env: Env<'_>, _: &[OsString]) -> Option<Command> {
    let shell = var(env, "ComSpec").filter(|shell| Path::new(shell).is_absolute())?;
    let mut command = Command::new(shell);
    command.creation_flags(CREATE_NEW_CONSOLE);
    Some(command)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::super::terminal_in;
    use crate::test_support::env;

    #[test]
    fn windows_opens_the_console_shell_in_the_folder() {
        let folder = std::path::Path::new(r"C:\Users\ada\chat");
        let shell = env(&[("ComSpec", r"C:\Windows\System32\cmd.exe")]);
        let command = terminal_in(folder, &shell, &[r"C:\weacon.exe".into()]).unwrap();
        assert_eq!(
            command.get_program(),
            OsStr::new(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_current_dir(), Some(folder));
        assert!(terminal_in(folder, &env(&[]), &[]).is_none());
        let relative = env(&[("ComSpec", "cmd.exe")]);
        assert!(terminal_in(folder, &relative, &[]).is_none());
    }
}
