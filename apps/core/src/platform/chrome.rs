use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
use super::{Env, home_dir, var};

/// The name Chrome's extension connects to.
pub(crate) const CHROME_HOST: &str = "io.weacon.core";

/// Registers `manifest` as this user's native host for Chrome and Chromium, and returns where
/// it was written.
#[cfg(not(windows))]
pub(crate) fn register_chrome_host(
    env: Env<'_>,
    _state_dir: &Path,
    manifest: &[u8],
) -> io::Result<Vec<PathBuf>> {
    let dirs = chrome_host_dirs(env).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    dirs.into_iter()
        .map(|dir| {
            fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{CHROME_HOST}.json"));
            fs::write(&path, manifest)?;
            Ok(path)
        })
        .collect()
}

// Where Chrome and Chromium look for a user's native hosts on macOS and Linux.
#[cfg(not(windows))]
fn chrome_host_dirs(env: Env<'_>) -> Option<[PathBuf; 2]> {
    let home = home_dir(env)?;
    let (chrome, chromium, base) = if cfg!(target_os = "macos") {
        (
            "Google/Chrome",
            "Chromium",
            home.join("Library/Application Support"),
        )
    } else {
        let config = var(env, "XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        ("google-chrome", "chromium", config)
    };
    Some([chrome, chromium].map(|browser| base.join(browser).join("NativeMessagingHosts")))
}

#[cfg(windows)]
pub(crate) use windows::register_chrome_host;

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::{Path, PathBuf};

    use super::super::{Env, var};
    use super::register_with;

    /// Registers `manifest` as this user's native host for Chrome and Chromium: Windows finds it
    /// through the registry, which points at a copy in `state_dir`.
    pub(crate) fn register_chrome_host(
        env: Env<'_>,
        state_dir: &Path,
        manifest: &[u8],
    ) -> io::Result<Vec<PathBuf>> {
        // By its full path, so a stray reg.exe beside weacon.exe isn't run instead.
        let reg = var(env, "SystemRoot")
            .map(|root| PathBuf::from(root).join(r"System32\reg.exe"))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        register_with(state_dir, manifest, |args| {
            let status = std::process::Command::new(&reg)
                .args(args)
                .stdout(std::process::Stdio::null())
                .status()?;
            Ok(status.success())
        })
    }
}

// Writes the manifest into `state_dir` and points each browser's key at it with `reg`'s args.
#[cfg(any(windows, test))]
fn register_with(
    state_dir: &Path,
    manifest: &[u8],
    reg: impl Fn(&[&std::ffi::OsStr]) -> io::Result<bool>,
) -> io::Result<Vec<PathBuf>> {
    super::private_dir(state_dir)?;
    let path = state_dir.join(format!("{CHROME_HOST}.json"));
    fs::write(&path, manifest)?;
    for browser in [r"Google\Chrome", "Chromium"] {
        let key = format!(r"HKCU\Software\{browser}\NativeMessagingHosts\{CHROME_HOST}");
        let args =
            ["add", key.as_str(), "/ve", "/t", "REG_SZ", "/f", "/d"].map(std::ffi::OsStr::new);
        if !reg(&[args.as_slice(), &[path.as_os_str()]].concat())? {
            return Err(io::Error::other("reg add failed"));
        }
    }
    Ok(vec![path])
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn on_windows_the_registry_points_each_browser_at_the_manifest() {
        let dir = temp_dir("chrome-registry");
        let calls = RefCell::new(Vec::new());
        let reg = |args: &[&std::ffi::OsStr]| {
            calls.borrow_mut().push(
                args.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            );
            Ok(true)
        };

        let written = register_with(&dir, b"{}", reg).unwrap();

        let manifest = dir.join("io.weacon.core.json");
        assert_eq!(written, std::slice::from_ref(&manifest));
        assert_eq!(fs::read(&manifest).unwrap(), b"{}");
        let path = manifest.to_string_lossy().into_owned();
        let calls = calls.into_inner();
        assert_eq!(calls.len(), 2);
        for (call, browser) in calls.iter().zip([r"Google\Chrome", "Chromium"]) {
            let key = format!(r"HKCU\Software\{browser}\NativeMessagingHosts\io.weacon.core");
            assert_eq!(
                call,
                &["add", &key, "/ve", "/t", "REG_SZ", "/f", "/d", &path]
            );
        }
        let failed = register_with(&dir, b"{}", |_| Ok(false)).unwrap_err();
        assert_eq!(failed.kind(), io::ErrorKind::Other);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_hosts_go_in_the_config_folder() {
        use crate::test_support::env;

        let dirs = |vars: &[(&str, &str)]| chrome_host_dirs(&env(vars));
        let home = ("HOME", "/home/ada");

        assert_eq!(
            dirs(&[home]),
            Some([
                PathBuf::from("/home/ada/.config/google-chrome/NativeMessagingHosts"),
                PathBuf::from("/home/ada/.config/chromium/NativeMessagingHosts"),
            ])
        );
        assert_eq!(
            dirs(&[home, ("XDG_CONFIG_HOME", "/cfg")]).map(|[chrome, _]| chrome),
            Some(PathBuf::from("/cfg/google-chrome/NativeMessagingHosts"))
        );
        assert_eq!(
            dirs(&[home, ("XDG_CONFIG_HOME", "rel")]).map(|[chrome, _]| chrome),
            Some(PathBuf::from(
                "/home/ada/.config/google-chrome/NativeMessagingHosts"
            ))
        );
        assert_eq!(dirs(&[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_manifest_is_written_for_each_browser() {
        use crate::test_support::env;

        let dir = temp_dir("chrome-host");
        let home = dir.join("home");
        let vars = env(&[("HOME", home.to_str().unwrap())]);

        let written = register_chrome_host(&vars, &dir, b"{}").unwrap();

        assert_eq!(written.len(), 2);
        for path in &written {
            assert!(path.starts_with(&home), "{}", path.display());
            assert!(path.ends_with("NativeMessagingHosts/io.weacon.core.json"));
            assert_eq!(fs::read(path).unwrap(), b"{}");
        }
        assert_eq!(
            register_chrome_host(&env(&[]), &dir, b"{}")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
