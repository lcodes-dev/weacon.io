//! The git module's own state files: each a JSON list, read whole and written whole.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The list in `file`; none before the first is written.
pub(super) fn load<T: DeserializeOwned>(file: &Path) -> io::Result<Vec<T>> {
    match fs::read(file) {
        Ok(json) => Ok(serde_json::from_slice(&json)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub(super) fn save<T: Serialize>(file: &Path, items: &[T]) -> io::Result<()> {
    weacon_appkit::write_whole(file, &serde_json::to_vec(items)?)
}

/// A hold on changing `file`, kept until dropped, so no other process changes it meanwhile.
/// While another holds it, this fails as `WouldBlock`.
pub(super) fn hold(file: &Path) -> io::Result<File> {
    let lock = file.with_extension("lock");
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent)?;
    }
    let held = File::create(lock)?;
    held.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        TryLockError::Error(error) => error,
    })?;
    Ok(held)
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn a_list_comes_back_as_saved_and_none_is_empty() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");

        assert_eq!(load::<u32>(&file).unwrap(), Vec::<u32>::new());
        save(&file, &[3_u32, 1]).unwrap();
        assert_eq!(load::<u32>(&file).unwrap(), [3, 1]);
    }

    #[test]
    fn a_file_is_held_by_one_at_a_time() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("state/list.json");

        let held = hold(&file).unwrap();
        assert_eq!(hold(&file).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        drop(held);
        hold(&file).unwrap();
    }

    #[test]
    fn a_file_that_cant_be_read_or_parsed_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("torn.json"), "[1,").unwrap();
        fs::create_dir(dir.join("folder.json")).unwrap();

        assert_eq!(
            load::<u32>(&dir.join("torn.json")).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        load::<u32>(&dir.join("folder.json")).unwrap_err();
    }
}
