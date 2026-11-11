use std::ffi::OsStr;
use std::path::PathBuf;

// A `.cmd` shim is left out on Windows: it runs under `cmd.exe`, so stopping a run would end
// the shell and leave the agent running.
const FILE_ENDING: &str = std::env::consts::EXE_SUFFIX;

/// The first runnable `name` in the folders of `path`, a `PATH` value. Folders that aren't
/// absolute are skipped, so nothing in a working folder is ever picked up.
pub(crate) fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(format!("{name}{FILE_ENDING}")))
        .find(|file| super::is_runnable(file))
}

#[cfg(test)]
mod tests {
    use std::env::join_paths;
    use std::fs;
    use std::path::Path;

    use weacon_testing::TempDir;

    use super::*;

    #[cfg(unix)]
    fn program(dir: &Path, name: &str, runnable: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let file = dir.join(name);
        fs::create_dir_all(dir).unwrap();
        fs::write(&file, "").unwrap();
        let mode = if runnable { 0o755 } else { 0o644 };
        fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        file
    }

    #[cfg(unix)]
    #[test]
    fn the_first_runnable_file_in_an_absolute_folder_is_found() {
        let dir = TempDir::new().unwrap();
        program(&dir.join("plain"), "claude", false);
        fs::create_dir_all(dir.join("folder/claude")).unwrap();
        let first = program(&dir.join("first"), "claude", true);
        program(&dir.join("second"), "claude", true);
        let codex = program(&dir.join("second"), "codex", true);
        let folders = ["plain", "folder", "first", "second"].map(|name| dir.join(name));
        let path = join_paths(folders).unwrap();

        assert_eq!(find_program("claude", &path), Some(first));
        assert_eq!(find_program("codex", &path), Some(codex));
        assert_eq!(find_program("amp", &path), None);
        assert_eq!(find_program("claude", OsStr::new("")), None);
    }

    #[cfg(unix)]
    #[test]
    fn folders_that_arent_absolute_are_skipped() {
        let dir = TempDir::new().unwrap();
        let name = format!("weacon-test-{}", std::process::id());
        let name = name.as_str();
        // Found from the working folder if a relative entry were followed.
        let here = std::env::current_dir().unwrap();
        let planted = program(&here.join(".dev/programs"), name, true);
        let real = program(&dir, name, true);
        let relative = ["", ".dev/programs", "./.dev/programs"].map(PathBuf::from);

        let only_relative = join_paths(&relative).unwrap();
        let then_absolute = join_paths(relative.iter().chain([&dir.to_path_buf()])).unwrap();

        assert!(planted.is_file());
        assert_eq!(find_program(name, &only_relative), None);
        assert_eq!(find_program(name, &then_absolute), Some(real));
        fs::remove_file(planted).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_program_is_found_and_a_command_shim_is_not() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("codex.cmd"), "").unwrap();
        fs::write(dir.join("claude.cmd"), "").unwrap();
        fs::write(dir.join("claude.exe"), "").unwrap();
        let path = join_paths([PathBuf::from("."), dir.to_path_buf()]).unwrap();

        assert_eq!(find_program("claude", &path), Some(dir.join("claude.exe")));
        assert_eq!(find_program("codex", &path), None);
    }
}
