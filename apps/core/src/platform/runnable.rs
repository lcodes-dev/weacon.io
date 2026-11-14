use std::io;
use std::path::Path;

/// Lets anyone run `file`, and only its owner change it.
#[cfg(unix)]
pub(crate) fn make_runnable(file: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
}

/// Windows runs any file whose name ends in `.exe`.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the same call as on Unix, where it can fail"
)]
pub(crate) fn make_runnable(_file: &Path) -> io::Result<()> {
    Ok(())
}
