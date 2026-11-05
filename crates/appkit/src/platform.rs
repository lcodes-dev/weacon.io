use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Opens `path` for appending, creating it readable only by this user.
///
/// # Errors
/// When the file can't be opened or created.
pub fn private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    // Readable too: Windows locks only a file opened for reading or writing.
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn new_files_are_private() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::temp_dir("private");
        let file = dir.join("weacon.log");
        drop(super::private_append(&file).unwrap());

        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
