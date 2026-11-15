//! Terminal.app, opened on a folder given as a path; it runs nothing it's given.

#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "macos")]
use crate::platform::Env;

#[cfg(target_os = "macos")]
pub(super) fn desktop_terminal(folder: &Path, _: Env<'_>, _: &[OsString]) -> Option<Command> {
    opened_by(
        "/System/Applications/Utilities/Terminal.app".as_ref(),
        folder,
    )
}

// `open -a <app> <folder>`, where the app is there; a folder is absolute, so never an option.
fn opened_by(app: &Path, folder: &Path) -> Option<Command> {
    if !app.is_dir() {
        return None;
    }
    let mut command = Command::new("/usr/bin/open");
    command.arg("-a").arg(app).arg(folder);
    Some(super::detached(command))
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn terminal_app_opens_on_the_folder_and_is_given_nothing_to_run() {
        let dir = TempDir::new().unwrap();
        let app = dir.join("Terminal.app");
        let folder = dir.join("chat");
        assert!(opened_by(&app, &folder).is_none());
        std::fs::create_dir(&app).unwrap();

        let command = opened_by(&app, &folder).unwrap();

        assert_eq!(command.get_program(), "/usr/bin/open");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-a".as_ref(), app.as_os_str(), folder.as_os_str()]);
    }
}
