//! The person's own terminal program, opened in a folder on their desktop: fixed arguments,
//! the folder and the core's own command as the only things that vary, and no shell between.

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod macos;
#[cfg(windows)]
mod windows;

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(all(unix, not(target_os = "macos")))]
use linux::desktop_terminal;
#[cfg(target_os = "macos")]
use macos::desktop_terminal;
#[cfg(windows)]
use windows::desktop_terminal;

use super::Env;

/// The command that opens this desktop's terminal in `folder`, running `run` in it where the
/// terminal takes a command without a shell; none for a folder or a program that isn't
/// absolute, or where there is no desktop terminal.
pub(crate) fn terminal_in(folder: &Path, env: Env<'_>, run: &[OsString]) -> Option<Command> {
    let absolute = |path: &OsString| Path::new(path).is_absolute();
    if !folder.is_absolute() || run.first().is_some_and(|program| !absolute(program)) {
        return None;
    }
    let mut command = desktop_terminal(folder, env, run)?;
    command
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env;

    #[test]
    fn a_folder_or_a_program_that_isnt_absolute_opens_nothing() {
        assert!(terminal_in(Path::new("chats/c-1"), &env(&[]), &[]).is_none());
        let relative = ["weacon", "resume"].map(OsString::from);
        let folder = std::env::temp_dir();
        assert!(terminal_in(&folder, &env(&[]), &relative).is_none());
    }
}
