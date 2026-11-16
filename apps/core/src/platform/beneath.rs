//! A plain file of a folder, opened only when it isn't a link.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// The file at `relative` in `root`, opened to read; a link, a folder, a pipe and a path that
/// isn't there are refused.
pub(crate) fn open_beneath(root: &Path, relative: &str) -> io::Result<File> {
    let path = root.join(relative);
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    File::open(path)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use weacon_testing::TempDir;

    use super::*;

    fn read(root: &Path, relative: &str) -> Option<String> {
        let mut text = String::new();
        open_beneath(root, relative)
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    }

    #[test]
    fn opens_a_plain_file_and_nothing_else() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/c.md"), "inside").unwrap();

        assert_eq!(read(&dir, "a/c.md").as_deref(), Some("inside"));
        assert_eq!(read(&dir, "a"), None);
        assert_eq!(read(&dir, "a/x.md"), None);
        if crate::platform::plant_link(&dir.join("a/c.md"), &dir.join("link.md")) {
            assert_eq!(read(&dir, "link.md"), None);
        }
    }
}
