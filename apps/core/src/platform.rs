use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Reads an environment variable; tests pass a fake.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// An environment variable, where an empty one counts as unset.
pub(crate) fn var(env: Env<'_>, key: &str) -> Option<OsString> {
    env(key).filter(|value| !value.is_empty())
}

pub(crate) fn home_dir(env: Env<'_>) -> Option<PathBuf> {
    var(env, "HOME").map(PathBuf::from)
}

/// Where the core keeps its state; `WEACON_STATE_DIR` overrides it.
pub(crate) fn state_dir(env: Env<'_>) -> Option<PathBuf> {
    if let Some(dir) = var(env, "WEACON_STATE_DIR") {
        return Some(dir.into());
    }
    let home = home_dir(env)?;
    if cfg!(target_os = "macos") {
        return Some(home.join("Library/Application Support/Weacon"));
    }
    // The XDG spec says to ignore a relative path.
    let base = var(env, "XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"));
    Some(base.join("weacon"))
}

/// Creates `dir` and its parents, and makes `dir` readable only by this user.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Opens `path` for appending, creating it readable only by this user.
pub(crate) fn private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env, temp_dir};

    #[test]
    fn the_state_folder_can_be_set() {
        let env = env(&[("WEACON_STATE_DIR", "/srv/weacon"), ("HOME", "/home/ada")]);

        assert_eq!(state_dir(&env), Some(PathBuf::from("/srv/weacon")));
        assert_eq!(home_dir(&env), Some(PathBuf::from("/home/ada")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_the_state_folder_follows_xdg() {
        let state = |vars: &[(&str, &str)]| state_dir(&env(vars));
        let home = ("HOME", "/home/ada");
        let default = Some(PathBuf::from("/home/ada/.local/state/weacon"));

        assert_eq!(state(&[home]), default);
        assert_eq!(
            state(&[home, ("XDG_STATE_HOME", "/data/state")]),
            Some(PathBuf::from("/data/state/weacon"))
        );
        assert_eq!(state(&[home, ("XDG_STATE_HOME", "state")]), default);
        assert_eq!(state(&[home, ("WEACON_STATE_DIR", "")]), default);
        assert_eq!(state(&[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn folders_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("private");
        let nested = dir.join("state");
        let file = nested.join("weacon.log");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&nested).unwrap();
        private_dir(&dir).unwrap();
        drop(private_append(&file).unwrap());
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&nested), 0o700);
        assert_eq!(mode(&file), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }
}
