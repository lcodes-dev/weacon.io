//! The Recycle Bin, as Windows' Explorer keeps it.

use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::time::{SystemTime, UNIX_EPOCH};

use weacon_client::platform::windows::current_user;

use super::{Trashed, place_first};
use crate::platform::Env;

// Seconds from 1601, where Windows' clock starts, to 1970.
const EPOCH_GAP: u64 = 11_644_473_600;

/// Moves `path`, an absolute path, into this user's Recycle Bin on its drive, as Explorer
/// does: the item as `$R…` beside a `$I…` file saying where it was. Fails, leaving `path`
/// alone, where the drive has no Recycle Bin for this user.
pub(crate) fn trash(path: &Path, _: Env<'_>, now: SystemTime) -> io::Result<Trashed> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    path.symlink_metadata()?;
    let (drive, plain) = on_drive(path)?;
    let bin = PathBuf::from(format!("{drive}:\\$Recycle.Bin")).join(current_user()?);
    if !bin.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    trash_into(path, &plain, &bin, now)
}

// The drive letter `path` is on, and `path` written plainly from it, as `C:\…`.
pub(super) fn on_drive(path: &Path) -> io::Result<(char, PathBuf)> {
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
        return Err(io::ErrorKind::Unsupported.into());
    };
    let drive = char::from(letter).to_ascii_uppercase();
    Ok((
        drive,
        PathBuf::from(format!("{drive}:")).join(parts.as_path()),
    ))
}

pub(super) fn trash_into(
    path: &Path,
    plain: &Path,
    bin: &Path,
    now: SystemTime,
) -> io::Result<Trashed> {
    let mut open = fs::OpenOptions::new();
    open.write(true).create_new(true);
    let ending = path.extension().unwrap_or_default();
    place_first(path, &info(plain, now)?, &open, |_| {
        let id = name_id(weacon_appkit::random_id());
        let named = |mark: &str| bin.join(format!("{mark}{id}")).with_extension(ending);
        (named("$R"), Some(named("$I")))
    })
}

// Six capitals or digits, as Explorer names its items.
fn name_id(random: u128) -> String {
    let mut rest = random;
    (0..6)
        .map(|_| {
            let digit = u32::try_from(rest % 36).unwrap_or_default();
            rest /= 36;
            char::from_digit(digit, 36)
                .unwrap_or('0')
                .to_ascii_uppercase()
        })
        .collect()
}

// A `$I` file, version 2: the version, a size (0: not counted), the time it was trashed in
// 100 ns since 1601, and the original path's UTF-16 length and text with its NUL.
pub(super) fn info(plain: &Path, now: SystemTime) -> io::Result<Vec<u8>> {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let ticks = since
        .as_secs()
        .checked_add(EPOCH_GAP)
        .and_then(|secs| secs.checked_mul(10_000_000))
        .and_then(|ticks| ticks.checked_add(u64::from(since.subsec_nanos() / 100)))
        .ok_or(io::ErrorKind::InvalidInput)?;
    let text: Vec<u16> = plain.as_os_str().encode_wide().chain([0]).collect();
    let len = u32::try_from(text.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut bytes = Vec::new();
    bytes.extend(2_u64.to_le_bytes());
    bytes.extend(0_u64.to_le_bytes());
    bytes.extend(ticks.to_le_bytes());
    bytes.extend(len.to_le_bytes());
    bytes.extend(text.iter().flat_map(|unit| unit.to_le_bytes()));
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use weacon_testing::TempDir;

    use super::*;
    use crate::platform::trash::{folder, restore};

    #[test]
    fn on_windows_items_go_to_the_recycle_bin_with_an_info_file_each() {
        let dir = TempDir::new().unwrap();
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let item = folder(&dir, "c-a.site");
        let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        let one = trash_into(&item, &item, &bin, when).unwrap();
        let two = trash_into(&folder(&dir, "c-a.site"), &item, &bin, when).unwrap();

        assert!(!item.exists());
        assert_ne!(one.item, two.item);
        let name = one.item.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("$R") && name.len() == 13, "{name}");
        assert_eq!(one.item.extension(), Some("site".as_ref()));
        assert_eq!(
            one.info
                .as_ref()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            name.replacen("$R", "$I", 1)
        );
        assert_eq!(
            fs::read(one.info.as_ref().unwrap()).unwrap(),
            info(&item, when).unwrap()
        );
        assert_eq!(
            fs::read_to_string(one.item.join("work.txt")).unwrap(),
            "c-a.site"
        );

        restore(&one, &item).unwrap();
        assert!(item.join("work.txt").exists() && !one.info.unwrap().exists());
    }

    #[test]
    fn on_windows_an_info_file_says_where_and_when() {
        let when = UNIX_EPOCH + Duration::from_millis(1_500);
        let info = info(Path::new(r"C:\a"), when).unwrap();

        let ticks = 11_644_473_600_u64 * 10_000_000 + 15_000_000;
        let mut expected = Vec::new();
        expected.extend(2_u64.to_le_bytes());
        expected.extend(0_u64.to_le_bytes());
        expected.extend(ticks.to_le_bytes());
        expected.extend(5_u32.to_le_bytes());
        expected.extend([b'C', 0, b':', 0, b'\\', 0, b'a', 0, 0, 0]);
        assert_eq!(info, expected);
    }

    #[test]
    fn on_windows_only_paths_on_a_drive_have_a_recycle_bin() {
        let (drive, plain) = on_drive(Path::new(r"\\?\c:\work\c-a")).unwrap();
        assert_eq!((drive, plain), ('C', PathBuf::from(r"C:\work\c-a")));
        let error = on_drive(Path::new(r"\\server\share\c-a")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
