use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(all(windows, test))]
pub(crate) use windows::Server;
#[cfg(windows)]
pub(crate) use windows::connect;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::path::Path;

    use tokio::net::UnixStream as Stream;

    /// Connects to the daemon's socket at `socket`.
    pub(crate) async fn connect(socket: &Path) -> io::Result<Stream> {
        Stream::connect(socket).await
    }

    /// A stand-in for the daemon's listener, for the tests.
    #[cfg(test)]
    pub(crate) struct Server(tokio::net::UnixListener);

    #[cfg(test)]
    impl Server {
        pub(crate) fn bind(socket: &Path) -> io::Result<Self> {
            tokio::net::UnixListener::bind(socket).map(Self)
        }

        pub(crate) async fn accept(self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Windows has no safe API for access lists or token users"
)]
pub mod windows;

/// Reads an environment variable; tests pass a fake.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// An environment variable, where an empty one counts as unset.
pub fn var(env: Env<'_>, key: &str) -> Option<OsString> {
    env(key).filter(|value| !value.is_empty())
}

pub fn home_dir(env: Env<'_>) -> Option<PathBuf> {
    var(env, if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Where the core keeps its state; `WEACON_STATE_DIR` overrides it.
pub fn state_dir(env: Env<'_>) -> Option<PathBuf> {
    if let Some(dir) = var(env, "WEACON_STATE_DIR") {
        // A relative path would differ between commands started in different folders.
        return std::path::absolute(dir).ok();
    }
    if cfg!(windows) {
        return var(env, "LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Weacon"));
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

/// The daemon's pipe name for `id`.
#[must_use]
pub fn pipe_name(id: u128) -> String {
    format!(r"\\.\pipe\weacon-{id:032x}")
}

/// Whether `name` is a pipe name the daemon could have made, so no network path is opened.
#[cfg_attr(
    all(unix, not(test)),
    expect(dead_code, reason = "only Windows connects by pipe name")
)]
fn is_pipe_name(name: &str) -> bool {
    name.strip_prefix(r"\\.\pipe\weacon-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        move |key| vars.get(key).cloned()
    }

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
    fn only_local_weacon_pipes_are_opened() {
        let good = pipe_name(0x0a0a_0a0a_0a0a_0a0a_0a0a_0a0a_0a0a_0a0a);

        assert_eq!(good, format!(r"\\.\pipe\weacon-{}", "0a".repeat(16)));
        assert!(is_pipe_name(&good));
        assert!(is_pipe_name(&pipe_name(1)));
        for bad in [
            format!(r"\\host\pipe\weacon-{}", "0a".repeat(16)),
            format!(r"\\.\pipe\weacon-{}", "0A".repeat(16)),
            format!(r"\\.\pipe\weacon-{}", "0a".repeat(15)),
            format!(r"\\.\pipe\other-{}", "0a".repeat(16)),
            format!("{good}\n"),
        ] {
            assert!(!is_pipe_name(&bad), "{bad}");
        }
    }
}
