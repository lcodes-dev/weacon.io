//! Installs a release's build over the running binary: stage it beside, check it, swap it in
//! keeping the old one, and put the old one back when the new one doesn't come up. The files
//! beside the binary have fixed names, so the caller holds one update lock from `stage` to the
//! end.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use weacon_observability::Fault;
use weacon_protocol::{Commit, ReleaseBuild, Sha256 as Sha256Hex, code};

use crate::platform;

/// The running binary and the two files an update keeps beside it.
#[derive(Debug, PartialEq, Eq)]
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
    let size = io::copy(&mut body, &mut out).map_err(download_failed)?;
    out.sync_all().map_err(download_failed)?;
    let digest: [u8; 32] = Sha256::digest(fs::read(file).map_err(download_failed)?).into();
    if size != build.size || build.sha256 != Sha256Hex::from_bytes(digest) {
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

/// Checks that the staged binary is the release's build from its `--version` answer, which
/// names its commit as `weacon_version::Version` prints it. A wrong one is removed.
pub(crate) fn probe(
    paths: &Paths,
    commit: &Commit,
    answer: io::Result<String>,
) -> Result<(), Fault> {
    let fault = Fault::new(
        code!("update.wrong_build"),
        "The downloaded build isn't the release's",
    );
    let fault = match answer {
        Ok(text) if text.contains(&format!("({})", commit.as_str())) => return Ok(()),
        Ok(_) => fault,
        Err(error) => fault.because(error),
    };
    remove(&paths.new);
    Err(fault)
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

/// Puts the old binary back after the new one didn't come up once restarted; the error says
/// whether that worked. The new one is moved aside first, so this works while it still runs.
pub(crate) fn roll_back(paths: &Paths) -> Fault {
    remove(&paths.new);
    if let Err(error) = fs::rename(&paths.exe, &paths.new) {
        return rollback_failed(error);
    }
    if let Err(error) = fs::rename(&paths.old, &paths.exe) {
        drop(fs::rename(&paths.new, &paths.exe));
        return rollback_failed(error);
    }
    remove(&paths.new);
    Fault::new(
        code!("update.unhealthy"),
        "The new build didn't start, so the old one is back",
    )
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

    use weacon_protocol::{BuildTarget, DownloadUrl};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::Broken;

    const RELEASE: &str = "0123456789abcdef0123456789abcdef01234567";

    fn build_of(bytes: &[u8]) -> ReleaseBuild {
        ReleaseBuild {
            target: BuildTarget::try_from("x86_64-unknown-linux-musl".to_owned()).unwrap(),
            url: DownloadUrl::try_from("https://example.com/weacon".to_owned()).unwrap(),
            size: u64::try_from(bytes.len()).unwrap(),
            sha256: Sha256Hex::from_bytes(Sha256::digest(bytes).into()),
        }
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
        let answer = Ok(format!("weacon 1.2.3 ({RELEASE})\n"));
        probe(&paths, &commit(), answer).unwrap();
        swap(&paths).unwrap();
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
    fn a_download_is_checked_against_its_releases_sha256() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        let sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let build = ReleaseBuild {
            sha256: Sha256Hex::try_from(sha256.to_owned()).unwrap(),
            ..build_of(b"abc")
        };
        stage(&paths, &mut Cursor::new("abc"), &build).unwrap();
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
            let fault = probe(&paths, &commit(), Ok(answer.to_owned())).unwrap_err();
            assert_eq!(fault.code().as_str(), "update.wrong_build", "{answer}");
            assert_eq!(files(&dir), [pair("weacon", "old")], "{answer}");
        }
    }

    #[test]
    fn a_build_that_does_not_run_is_removed() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        fs::write(&paths.new, "new").unwrap();
        let fails = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let fault = probe(&paths, &commit(), fails).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.wrong_build");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn an_unhealthy_start_puts_the_old_binary_back() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        fs::write(&paths.new, "new").unwrap();
        swap(&paths).unwrap();
        let fault = roll_back(&paths);
        assert_eq!(fault.code().as_str(), "update.unhealthy");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn a_download_that_never_ends_is_cut_off() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        let mut endless = io::repeat(b'n');
        let fault = stage(&paths, &mut endless, &build_of(b"new")).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.corrupt");
        assert_eq!(files(&dir), [pair("weacon", "old")]);
    }

    #[test]
    fn a_rollback_without_the_old_binary_says_so() {
        let dir = TempDir::new().unwrap();
        let paths = installed(&dir);
        let fault = roll_back(&paths);
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
}
