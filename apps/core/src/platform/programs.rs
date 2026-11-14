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

/// Writes an empty program `name` into `dir`, with Unix permissions `mode`.
#[cfg(test)]
pub(crate) fn plant_program(dir: &std::path::Path, name: &str, mode: u32) -> PathBuf {
    let file = dir.join(format!("{name}{FILE_ENDING}"));
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&file, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    let _ = mode;
    file
}

/// Writes a shell script `name` running `body` into `dir`, from a child shell, so no other
/// test's fork holds it open for writing when it runs.
#[cfg(test)]
#[cfg(unix)]
pub(crate) fn plant_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let file = dir.join(name);
    std::fs::create_dir_all(dir).unwrap();
    let script = format!("#!/bin/sh\n{body}\n");
    let written = std::process::Command::new("sh")
        .args([
            "-c",
            "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
            "sh",
            &script,
        ])
        .arg(&file)
        .status()
        .unwrap();
    assert!(written.success());
    file
}

#[cfg(test)]
mod tests {
    use std::env::join_paths;
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    // Planted outside a temporary folder.
    struct Planted(PathBuf);

    impl Drop for Planted {
        fn drop(&mut self) {
            fs::remove_file(&self.0).unwrap_or_default();
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_first_file_this_user_may_run_in_an_absolute_folder_is_found() {
        use std::os::unix::fs::MetadataExt;

        let dir = TempDir::new().unwrap();
        plant_program(&dir.join("plain"), "claude", 0o644);
        let others = plant_program(&dir.join("others"), "claude", 0o011);
        fs::create_dir_all(dir.join("folder/claude")).unwrap();
        let first = plant_program(&dir.join("first"), "claude", 0o700);
        plant_program(&dir.join("second"), "claude", 0o755);
        let codex = plant_program(&dir.join("second"), "codex", 0o755);
        let folders = ["plain", "folder", "first", "second"].map(|name| dir.join(name));
        let path = join_paths(folders).unwrap();
        // Root may run anything with an execute bit.
        let root = fs::metadata(&others).unwrap().uid() == 0;

        assert_eq!(find_program("claude", &path), Some(first));
        assert_eq!(find_program("codex", &path), Some(codex));
        assert_eq!(find_program("amp", &path), None);
        assert_eq!(find_program("claude", OsStr::new("")), None);
        assert_eq!(crate::platform::is_runnable(&others), root);
    }

    #[test]
    fn folders_that_arent_absolute_are_skipped() {
        let dir = TempDir::new().unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        // Found from the working folder if a relative entry were followed.
        let here = std::env::current_dir().unwrap();
        let planted = Planted(plant_program(&here.join(".dev/programs"), name, 0o755));
        let real = plant_program(&dir, name, 0o755);
        let relative = ["", ".dev/programs", "./.dev/programs"].map(PathBuf::from);

        let only_relative = join_paths(&relative).unwrap();
        let then_absolute = join_paths(relative.iter().chain([&dir.to_path_buf()])).unwrap();

        assert!(planted.0.is_file());
        assert_eq!(find_program(name, &only_relative), None);
        assert_eq!(find_program(name, &then_absolute), Some(real));
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
