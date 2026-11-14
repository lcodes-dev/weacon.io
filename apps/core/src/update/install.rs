//! Installs a release's build over the running binary: stage it beside, check it, swap it in
//! keeping the old one, and put the old one back when the new one doesn't come up.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use weacon_observability::Fault;
use weacon_protocol::{Commit, ReleaseBuild, code};

use crate::platform;

/// The running binary and the two files an update keeps beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Paths {
    pub(crate) exe: PathBuf,
    pub(crate) new: PathBuf,
    pub(crate) old: PathBuf,
}

impl Paths {
    /// `weacon.new` and `weacon.old` beside `weacon`; on Windows `weacon.new.exe` and
    /// `weacon.old.exe`, so both still run.
    pub(crate) fn beside(exe: &Path) -> Option<Self> {
        let stem = exe.file_stem()?.to_str()?;
        let ext = exe.extension().and_then(|ext| ext.to_str());
        let named = |tag: &str| match ext {
            Some(ext) => exe.with_file_name(format!("{stem}.{tag}.{ext}")),
            None => exe.with_file_name(format!("{stem}.{tag}")),
        };
        Some(Self {
            exe: exe.to_owned(),
            new: named("new"),
            old: named("old"),
        })
    }
}

/// Writes the download in `body` to `paths.new`, then checks its size and SHA-256 against
/// `build`. Nothing is left behind when it fails.
pub(crate) fn stage(paths: &Paths, body: &mut dyn Read, build: &ReleaseBuild) -> Result<(), Fault> {
    let result = write_checked(&paths.new, body, build);
    if result.is_err() {
        remove(&paths.new);
    }
    result
}

fn write_checked(file: &Path, body: &mut dyn Read, build: &ReleaseBuild) -> Result<(), Fault> {
    remove(file);
    let mut out = File::create_new(file).map_err(|error| {
        Fault::new(
            code!("update.not_writable"),
            "The folder of the weacon binary can't be written to",
        )
        .because(error)
    })?;
    // One byte more than the build's size shows a download that is too long.
    let mut body = body.take(build.size.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let read = body.read(&mut buffer).map_err(download_failed)?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        hasher.update(chunk);
        out.write_all(chunk).map_err(download_failed)?;
        size = size.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    out.sync_all().map_err(download_failed)?;
    if size != build.size || hex(&hasher.finalize()) != build.sha256.as_str() {
        return Err(Fault::new(
            code!("update.corrupt"),
            "The downloaded build doesn't match its release",
        ));
    }
    platform::make_runnable(file).map_err(download_failed)
}

fn download_failed(error: io::Error) -> Fault {
    Fault::new(
        code!("update.download_failed"),
        "The build couldn't be downloaded",
    )
    .because(error)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xf])
        .filter_map(|digit| char::from_digit(digit.into(), 16))
        .collect()
}

/// Checks that the staged binary is the release's build: `version_of` runs it with
/// `--version`, and its answer must name `commit`. A wrong one is removed.
pub(crate) fn probe(
    paths: &Paths,
    commit: &Commit,
    version_of: &mut dyn FnMut(&Path) -> io::Result<String>,
) -> Result<(), Fault> {
    let answer = version_of(&paths.new);
    if answer
        .as_ref()
        .is_ok_and(|text| text.contains(&format!("({})", commit.as_str())))
    {
        return Ok(());
    }
    remove(&paths.new);
    let fault = Fault::new(
        code!("update.wrong_build"),
        "The downloaded build isn't the release's",
    );
    Err(match answer {
        Err(error) => fault.because(error),
        Ok(_) => fault,
    })
}

/// Moves the running binary to `paths.old` and the staged one into its place. Renaming works
/// while the binary runs, even on Windows, which can't overwrite it. When the second rename
/// fails, the running binary goes back.
pub(crate) fn swap(paths: &Paths) -> Result<(), Fault> {
    let failed = |error| {
        Fault::new(
            code!("update.swap_failed"),
            "The new build couldn't be put in place",
        )
        .because(error)
    };
    remove(&paths.old);
    if let Err(error) = fs::rename(&paths.exe, &paths.old) {
        remove(&paths.new);
        return Err(failed(error));
    }
    if let Err(error) = fs::rename(&paths.new, &paths.exe) {
        remove(&paths.new);
        fs::rename(&paths.old, &paths.exe).map_err(rollback_failed)?;
        return Err(failed(error));
    }
    Ok(())
}

/// Ends an update once the new binary has restarted: when `healthy` says it didn't come up,
/// the old binary goes back in its place.
pub(crate) fn confirm(paths: &Paths, healthy: bool) -> Result<(), Fault> {
    if healthy {
        return Ok(());
    }
    let unhealthy = Fault::new(
        code!("update.unhealthy"),
        "The new build didn't start, so the old one is back",
    );
    fs::rename(&paths.old, &paths.exe).map_err(rollback_failed)?;
    Err(unhealthy)
}

fn rollback_failed(error: io::Error) -> Fault {
    Fault::new(
        code!("update.rollback_failed"),
        "The update failed, and the old build couldn't be put back",
    )
    .because(error)
}

fn remove(file: &Path) {
    // Missing is the goal; any other failure shows up at the next step that needs the file.
    drop(fs::remove_file(file));
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use weacon_testing::TempDir;

    use super::*;

    const RELEASE: &str = "0123456789abcdef0123456789abcdef01234567";

    fn build_of(bytes: &[u8]) -> ReleaseBuild {
        serde_json::from_value(serde_json::json!({
            "target": "x86_64-unknown-linux-musl",
            "url": "https://example.com/weacon",
            "size": bytes.len(),
            "sha256": hex(&Sha256::digest(bytes)),
        }))
        .unwrap()
    }

    fn commit() -> Commit {
        Commit::try_from(RELEASE.to_owned()).unwrap()
    }

    /// A folder with a running binary `weacon` that holds `old`.
    fn installed(dir: &TempDir) -> Paths {
        let paths = Paths::beside(&dir.join("weacon")).unwrap();
        fs::write(&paths.exe, "old").unwrap();
        paths
    }

    fn files(dir: &Path) -> Vec<(String, String)> {
        let mut files: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name, fs::read_to_string(entry.path()).unwrap())
            })
            .collect();
        files.sort();
        files
    }

    fn pair(name: &str, text: &str) -> (String, String) {
        (name.to_owned(), text.to_owned())
    }

    fn version(text: &'static str) -> impl FnMut(&Path) -> io::Result<String> {
        move |_| Ok(text.to_owned())
    }

    #[test]
    fn the_staged_files_sit_beside_the_binary_and_still_run_on_windows() {
        let unix = Paths::beside(Path::new("/opt/bin/weacon")).unwrap();
        assert_eq!(unix.new, Path::new("/opt/bin/weacon.new"));
        assert_eq!(unix.old, Path::new("/opt/bin/weacon.old"));
        let windows = Paths::beside(Path::new("C:/Weacon/weacon.exe")).unwrap();
        assert_eq!(windows.new, Path::new("C:/Weacon/weacon.new.exe"));
        assert_eq!(windows.old, Path::new("C:/Weacon/weacon.old.exe"));
        assert_eq!(Paths::beside(Path::new("/")), None);
    }

    #[test]
    fn a_whole_update_keeps_the_old_binary_beside_the_new_one() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        let build = build_of(b"new");
        stage(&paths, &mut Cursor::new("new"), &build).unwrap();
        let mut ran = Vec::new();
        let mut version_of = |file: &Path| {
            ran.push(file.to_owned());
            Ok(format!("weacon 1.2.3 ({RELEASE})\n"))
        };
        probe(&paths, &commit(), &mut version_of).unwrap();
        swap(&paths).unwrap();
        confirm(&paths, true).unwrap();
        assert_eq!(ran, [paths.new.as_path()]);
        assert_eq!(
            files(&dir),
            [pair("weacon", "new"), pair("weacon.old", "old")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_staged_binary_can_run() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        stage(&paths, &mut Cursor::new("new"), &build_of(b"new")).unwrap();
        let mode = fs::metadata(&paths.new).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn a_download_that_differs_from_its_release_is_refused_and_removed() {
        let build = build_of(b"new");
        for body in ["nex", "ne", "new!", ""] {
            let dir = TempDir::new().unwrap();
            let paths = installed(&dir);
            let fault = stage(&paths, &mut Cursor::new(body), &build).unwrap_err();
            assert_eq!(fault.code().as_str(), "update.corrupt", "{body:?}");
            assert_eq!(files(&dir), [pair("weacon", "old")], "{body:?}");
        }
    }

    #[test]
    fn a_failed_download_leaves_nothing_behind() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::ConnectionReset))
            }
        }
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        let mut body = Cursor::new("ne").chain(Broken);
        let fault = stage(&paths, &mut body, &build_of(b"new")).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.download_failed");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn a_stale_staged_file_is_replaced() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        fs::write(&paths.new, "stale and longer").unwrap();
        stage(&paths, &mut Cursor::new("new"), &build_of(b"new")).unwrap();
        assert_eq!(fs::read_to_string(&paths.new).unwrap(), "new");
    }

    #[test]
    fn an_unwritable_folder_is_refused_before_downloading() {
        let dir = TempDir::new().unwrap();
        // A file where the folder should be fails even for root, unlike permissions.
        fs::write(dir.join("bin"), "").unwrap();
        let paths = Paths::beside(&dir.join("bin/weacon")).unwrap();
        let mut body = Cursor::new("new");
        let fault = stage(&paths, &mut body, &build_of(b"new")).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.not_writable");
        assert_eq!(body.position(), 0);
    }

    #[test]
    fn a_build_that_reports_another_commit_is_removed() {
        let other = "weacon 1.2.3 (1123456789abcdef0123456789abcdef01234567)";
        let short = "weacon 1.2.3 (0123456)";
        for answer in [other, short, RELEASE] {
            let dir = TempDir::new().unwrap();
            let paths = installed(&dir);
            fs::write(&paths.new, "new").unwrap();
            let fault = probe(&paths, &commit(), &mut version(answer)).unwrap_err();
            assert_eq!(fault.code().as_str(), "update.wrong_build", "{answer}");
            assert_eq!(files(&dir), [pair("weacon", "old")], "{answer}");
        }
    }

    #[test]
    fn a_build_that_does_not_run_is_removed() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        fs::write(&paths.new, "new").unwrap();
        let mut fails = |_: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let fault = probe(&paths, &commit(), &mut fails).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.wrong_build");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn an_unhealthy_start_puts_the_old_binary_back() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        fs::write(&paths.new, "new").unwrap();
        swap(&paths).unwrap();
        let fault = confirm(&paths, false).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.unhealthy");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn a_rollback_without_the_old_binary_says_so() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        let fault = confirm(&paths, false).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.rollback_failed");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn a_swap_replaces_an_earlier_old_binary() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        fs::write(&paths.old, "older").unwrap();
        fs::write(&paths.new, "new").unwrap();
        swap(&paths).unwrap();
        assert_eq!(
            files(&dir),
            [pair("weacon", "new"), pair("weacon.old", "old")]
        );
    }

    #[test]
    fn a_swap_that_cannot_place_the_new_binary_keeps_the_running_one() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        // Nothing staged, so the second rename fails.
        let fault = swap(&paths).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.swap_failed");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn a_swap_without_the_running_binary_removes_the_staged_one() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::beside(&dir.join("weacon")).unwrap();
        fs::write(&paths.new, "new").unwrap();
        let fault = swap(&paths).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.swap_failed");
        assert_eq!(files(&dir), []);
    }

    #[test]
    fn digests_are_lowercase_hex() {
        assert_eq!(hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }
}
