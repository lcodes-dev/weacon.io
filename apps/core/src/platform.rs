use std::ffi::OsString;
use std::path::PathBuf;

mod chrome;
mod procs;
mod programs;
mod spawn_policy;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub(crate) use chrome::{CHROME_HOST, register_chrome_host};
pub(crate) use procs::{PROCESSES, Process, processes};
pub(crate) use programs::find_program;
#[cfg(test)]
pub(crate) use programs::plant_program;
pub(crate) use spawn_policy::spawn_policy;
#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;

/// Reads an environment variable; tests pass a fake.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// An environment variable, where an empty one counts as unset.
pub(crate) fn var(env: Env<'_>, key: &str) -> Option<OsString> {
    env(key).filter(|value| !value.is_empty())
}

pub(crate) fn home_dir(env: Env<'_>) -> Option<PathBuf> {
    var(env, if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Whether `weacon service` can install the daemon here, as a systemd user unit.
pub(crate) const USER_SERVICES: bool = cfg!(target_os = "linux");

/// The XDG base folder in `key`, or `fallback` under the home folder.
pub(crate) fn xdg_dir(env: Env<'_>, key: &str, fallback: &str) -> Option<PathBuf> {
    // The XDG spec says to ignore a relative path.
    var(env, key)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home_dir(env).map(|home| home.join(fallback)))
}

/// Where the core keeps its state; `WEACON_STATE_DIR` overrides it.
pub(crate) fn state_dir(env: Env<'_>) -> Option<PathBuf> {
    if let Some(dir) = var(env, "WEACON_STATE_DIR") {
        // A relative path would differ between commands started in different folders.
        return std::path::absolute(dir).ok();
    }
    if cfg!(windows) {
        return var(env, "LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Weacon"));
    }
    if cfg!(target_os = "macos") {
        return home_dir(env).map(|home| home.join("Library/Application Support/Weacon"));
    }
    xdg_dir(env, "XDG_STATE_HOME", ".local/state").map(|dir| dir.join("weacon"))
}

/// The visible folder in the home folder where agents work, never in or above the state, so
/// no agent reaches that; `WEACON_WORK_DIR` overrides it, and a set `WEACON_STATE_DIR` moves it
/// beside that.
pub(crate) fn work_root(env: Env<'_>) -> Option<PathBuf> {
    if let Some(dir) = var(env, "WEACON_WORK_DIR") {
        return std::path::absolute(dir).ok();
    }
    // A state folder of its own, as per worktree, gets a work folder of its own beside it.
    if var(env, "WEACON_STATE_DIR").is_some() {
        let state = state_dir(env)?;
        let mut name = state.file_name()?.to_os_string();
        name.push("-work");
        return Some(state.with_file_name(name));
    }
    home_dir(env).map(|home| home.join("Weacon"))
}

/// A file or folder name for `id`, a token id, after `prefix`. A capital becomes `.` and its
/// small letter, so ids differing only in case don't share a name; the prefix keeps Windows'
/// device names, such as `CON`, out.
pub(crate) fn case_safe_name(prefix: &str, id: &str) -> String {
    let mut name = String::from(prefix);
    for letter in id.chars() {
        if letter.is_ascii_uppercase() {
            name.push('.');
        }
        name.push(letter.to_ascii_lowercase());
    }
    name
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::env;

    #[cfg(windows)]
    #[test]
    fn on_windows_the_state_folder_is_in_local_app_data() {
        let vars = env(&[
            ("LOCALAPPDATA", r"C:\Users\ada\AppData\Local"),
            ("USERPROFILE", r"C:\Users\ada"),
        ]);

        assert_eq!(
            state_dir(&vars),
            Some(PathBuf::from(r"C:\Users\ada\AppData\Local\Weacon"))
        );
        assert_eq!(home_dir(&vars), Some(PathBuf::from(r"C:\Users\ada")));
        assert_eq!(state_dir(&env(&[])), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_folder_can_be_set() {
        let set = env(&[("WEACON_STATE_DIR", "/srv/weacon"), ("HOME", "/home/ada")]);
        let relative = env(&[("WEACON_STATE_DIR", ".dev/state")]);

        assert_eq!(state_dir(&set), Some(PathBuf::from("/srv/weacon")));
        assert_eq!(home_dir(&set), Some(PathBuf::from("/home/ada")));
        assert_eq!(
            state_dir(&relative),
            Some(std::env::current_dir().unwrap().join(".dev/state"))
        );
    }

    #[test]
    fn the_work_folder_is_in_the_home_folder_unless_set() {
        let home = env(&[("HOME", "/home/ada"), ("USERPROFILE", "/home/ada")]);
        let set = env(&[("WEACON_WORK_DIR", "/srv/work"), ("HOME", "/home/ada")]);
        let own_state = env(&[("WEACON_STATE_DIR", "/srv/weacon"), ("HOME", "/home/ada")]);
        let relative = env(&[("WEACON_WORK_DIR", ".dev/work")]);

        assert_eq!(work_root(&home), Some(PathBuf::from("/home/ada/Weacon")));
        // Absolute as the system makes it: on Windows, on the current drive.
        let absolute = |path: &str| std::path::absolute(path).ok();
        assert_eq!(work_root(&set), absolute("/srv/work"));
        assert_eq!(work_root(&own_state), absolute("/srv/weacon-work"));
        assert_eq!(
            work_root(&relative),
            Some(std::env::current_dir().unwrap().join(".dev/work"))
        );
        assert_eq!(work_root(&env(&[])), None);
    }

    #[test]
    fn the_work_folder_is_never_in_or_above_the_state_folder() {
        let home = ("HOME", "/home/ada");
        let profile = ("USERPROFILE", "C:/Users/ada");
        let local = ("LOCALAPPDATA", "C:/Users/ada/AppData/Local");
        let systems: [&[(&str, &str)]; 4] = [
            &[home, profile, local],
            &[home, profile, local, ("XDG_STATE_HOME", "/home/ada")],
            &[home, profile, ("WEACON_STATE_DIR", "/home/ada/Weacon")],
            &[home, profile, ("WEACON_STATE_DIR", "/home/ada")],
        ];
        for vars in systems {
            let vars = env(vars);
            let (Some(state), Some(work)) = (state_dir(&vars), work_root(&vars)) else {
                panic!("no folders");
            };
            let apart = !work.starts_with(&state) && !state.starts_with(&work);
            assert!(apart, "{work:?} and {state:?} overlap");
        }
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

    #[test]
    fn free_space_is_read_for_a_folder_and_refused_for_a_missing_one() {
        let dir = TempDir::new().unwrap();

        assert!(free_space(&dir).unwrap() > 0);
        free_space(&dir.join("missing")).unwrap_err();
    }
}
