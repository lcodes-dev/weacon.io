use std::ffi::OsString;
// How far above the core agents and their tools are for the OOM killer, so it picks them first.
const TOOL_OOM_RAISE: i32 = 500;
const OOM_SCORE_MAX: i32 = 1000;

// Tools that would otherwise count the host's CPUs rather than the quota; the person's own
// settings win.
const CPU_COUNT_VARS: [&str; 5] = [
    "CARGO_BUILD_JOBS",
    "GOMAXPROCS",
    "OMP_NUM_THREADS",
    "PYTHON_CPU_COUNT",
    "RAYON_NUM_THREADS",
];

/// How agents start here; on Linux their tools go first when memory runs out and use only the
/// CPUs the core may.
pub(crate) fn spawn_policy(env: Env<'_>) -> SpawnPolicy {
    if !cfg!(target_os = "linux") {
        return SpawnPolicy::default();
    }
    // Counts the cgroup's CPU quota and affinity, not just the host's CPUs.
    let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    tool_policy(env, cpus, oom_score(Path::new("/proc/self/oom_score_adj")))
}

// None where the core can't change its score, as on WSL 1 or with procfs read-only; writing
// the same score back proves it can.
fn oom_score(file: &Path) -> Option<i32> {
    let score = fs::read_to_string(file).ok()?;
    fs::write(file, &score).ok()?;
    score.trim().parse().ok()
}

fn tool_policy(env: Env<'_>, cpus: usize, own_oom_score: Option<i32>) -> SpawnPolicy {
    let count = OsString::from(cpus.to_string());
    SpawnPolicy {
        env: CPU_COUNT_VARS
            .into_iter()
            .filter(|name| var(env, name).is_none())
            .map(|name| (name.into(), count.clone()))
            .collect(),
        oom_score_adj: own_oom_score
            .map(|own| own.saturating_add(TOOL_OOM_RAISE).min(OOM_SCORE_MAX)),
    }
}

use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use weacon_agents::SpawnPolicy;

mod chrome;
mod procs;
mod programs;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "chats remove their worktrees through it once wired in"
    )
)]
pub(crate) mod trash;

pub(crate) use chrome::{CHROME_HOST, register_chrome_host};
pub(crate) use procs::{PROCESSES, Process, processes};
pub(crate) use programs::find_program;
#[cfg(test)]
pub(crate) use programs::{plant_link, plant_program};

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

/// Creates `dir` and any missing parents readable only by this user; existing folders keep
/// their permissions.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    create_private(dir)
}

use std::fs;
#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    pub(crate) use tokio::net::UnixStream as Stream;

    /// The user a client must be.
    pub(crate) type Owner = u32;

    pub(crate) struct Listener(tokio::net::UnixListener);

    impl Listener {
        #[expect(
            clippy::needless_pass_by_ref_mut,
            reason = "Windows' listener swaps in a new pipe on each accept"
        )]
        pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
            Ok(self.0.accept().await?.0)
        }
    }

    /// Bytes this user may still write on the disk holding `dir`.
    #[expect(unsafe_code, reason = "statvfs has no safe API in std")]
    #[cfg_attr(
        target_os = "linux",
        expect(clippy::useless_conversion, reason = "f_bavail is u32 on macOS")
    )]
    pub(crate) fn free_space(dir: &Path) -> io::Result<u64> {
        use std::ffi::CString;
        use std::mem::MaybeUninit;
        use std::os::unix::ffi::OsStrExt;

        let path = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut stats = MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is NUL-terminated and `stats` is writable space for one statvfs.
        if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: statvfs returned 0, so it filled `stats`.
        let stats = unsafe { stats.assume_init() };
        Ok(u64::from(stats.f_bavail).saturating_mul(stats.f_frsize))
    }

    pub(crate) fn create_private(dir: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }

    /// Whether `file` is a file this user may run.
    #[expect(unsafe_code, reason = "faccessat has no safe API in std")]
    pub(crate) fn is_runnable(file: &Path) -> bool {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let Ok(path) = CString::new(file.as_os_str().as_bytes()) else {
            return false;
        };
        file.is_file()
            // SAFETY: `path` is NUL-terminated and outlives the call.
            && unsafe {
                libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS)
            } == 0
    }

    /// The user that owns `dir`, when only that user can reach it.
    pub(crate) fn private_owner(dir: &Path) -> io::Result<Option<Owner>> {
        let metadata = fs::metadata(dir)?;
        let private = metadata.is_dir() && weacon_appkit::owner_only(&metadata) == Some(true);
        Ok(private.then(|| metadata.uid()))
    }

    /// Listens on a socket at `path` that only this user can open.
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        let listener = tokio::net::UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Listener(listener))
    }

    pub(crate) fn is_owner(stream: &Stream, owner: Owner) -> bool {
        stream.peer_cred().is_ok_and(|cred| cred.uid() == owner)
    }
}

#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::io;
    use std::mem;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::NamedPipeServer;
    use weacon_client::platform::pipe_name;
    use weacon_client::platform::windows::{create_pipe, make_private};

    /// A client's pipe, disconnected when dropped: a pipe with a write still pending, as to a
    /// client that never reads, otherwise stays open until that write finishes.
    pub(crate) struct Stream(NamedPipeServer);

    impl Drop for Stream {
        fn drop(&mut self) {
            // A pipe already gone has nothing left to close.
            self.0.disconnect().unwrap_or_default();
        }
    }

    impl AsyncRead for Stream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    /// Nothing to compare: the pipe's access list already admits only this user.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct Owner;

    pub(crate) struct Listener {
        name: String,
        next: NamedPipeServer,
    }

    impl Listener {
        /// The next client; a new pipe instance waits for the one after.
        pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
            self.next.connect().await?;
            let after = create_pipe(&self.name, false)?;
            Ok(Stream(mem::replace(&mut self.next, after)))
        }
    }

    /// Bytes this user may still write on the disk holding `dir`.
    #[expect(unsafe_code, reason = "GetDiskFreeSpaceExW has no safe API in std")]
    pub(crate) fn free_space(dir: &Path) -> io::Result<u64> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
        let mut available = 0_u64;
        // SAFETY: `wide` is NUL-terminated, `available` is writable and the totals may be null.
        let done = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &raw mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if done == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(available)
    }

    /// Creates `dir`, making it private if it's new; parents get the access they inherit.
    pub(crate) fn create_private(dir: &Path) -> io::Result<()> {
        let new = !dir.exists();
        fs::DirBuilder::new().recursive(true).create(dir)?;
        if new {
            make_private(dir)?;
        }
        Ok(())
    }

    pub(crate) fn is_runnable(file: &Path) -> bool {
        file.is_file()
    }

    /// Makes `dir`, the core's own folder, private to this user, whatever it inherited.
    pub(crate) fn private_owner(dir: &Path) -> io::Result<Option<Owner>> {
        make_private(dir)?;
        Ok(Some(Owner))
    }

    /// Listens on a pipe with a fresh random name that only this user can open, and writes the
    /// name to `path` for clients to find.
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        let name = pipe_name(weacon_appkit::random_id());
        let next = create_pipe(&name, true)?;
        weacon_appkit::write_whole(path, name.as_bytes())?;
        Ok(Listener { name, next })
    }

    pub(crate) const fn is_owner(_: &Stream, _: Owner) -> bool {
        true
    }
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
    fn tools_get_the_cpu_count_and_a_higher_oom_score_than_the_core() {
        use std::collections::BTreeMap;

        let vars = env(&[("GOMAXPROCS", "8"), ("OMP_NUM_THREADS", "")]);
        let policy = tool_policy(&vars, 2, Some(100));
        let two = OsString::from("2");

        assert_eq!(
            policy.env,
            BTreeMap::from([
                ("CARGO_BUILD_JOBS".into(), two.clone()),
                ("OMP_NUM_THREADS".into(), two.clone()),
                ("PYTHON_CPU_COUNT".into(), two.clone()),
                ("RAYON_NUM_THREADS".into(), two),
            ])
        );
        assert_eq!(policy.oom_score_adj, Some(600));
        assert_eq!(tool_policy(&vars, 1, Some(700)).oom_score_adj, Some(1000));
        assert_eq!(tool_policy(&vars, 1, None).oom_score_adj, None);
    }

    #[test]
    fn the_cores_oom_score_is_read_when_there_is_one() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("oom_score_adj");
        fs::write(&file, "-200\n").unwrap();

        assert_eq!(oom_score(&file), Some(-200));
        assert_eq!(fs::read_to_string(&file).unwrap(), "-200\n");
        assert_eq!(oom_score(&dir.join("missing")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_score_that_cant_be_written_is_left_alone() {
        // Readable, but refuses writes even from root.
        assert_eq!(oom_score(Path::new("/proc/self/oom_score")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_agents_start_under_a_policy() {
        let policy = spawn_policy(&env(&[]));

        // None where this test's procfs is read-only.
        let own = oom_score(Path::new("/proc/self/oom_score_adj"));

        assert_eq!(policy.env.len(), CPU_COUNT_VARS.len());
        assert_eq!(policy.oom_score_adj, own.map(|own| (own + 500).min(1000)));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn elsewhere_agents_start_as_they_are() {
        assert_eq!(spawn_policy(&env(&[])), SpawnPolicy::default());
    }

    #[test]
    fn free_space_is_read_for_a_folder_and_refused_for_a_missing_one() {
        let dir = TempDir::new().unwrap();

        assert!(free_space(&dir).unwrap() > 0);
        free_space(&dir.join("missing")).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn new_folders_are_private_and_existing_ones_kept() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let nested = dir.join("state/logs");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&nested).unwrap();
        private_dir(&dir).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode(&dir), 0o755);
        assert_eq!(mode(&dir.join("state")), 0o700);
        assert_eq!(mode(&nested), 0o700);
    }
}
