//! What differs between operating systems.

#[cfg(unix)]
pub(crate) use unix::links;
#[cfg(windows)]
pub(crate) use windows::links;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    /// How many hard links `file` has.
    pub(crate) fn links(file: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(file)?.nlink())
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    /// How many hard links `file` has.
    #[expect(
        unsafe_code,
        reason = "std has no stable way to count a file's links on Windows"
    )]
    pub(crate) fn links(file: &Path) -> io::Result<u64> {
        let opened = std::fs::File::open(file)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle stays open for the call, and `info` is room for one structure.
        if unsafe { GetFileInformationByHandle(opened.as_raw_handle(), &raw mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(u64::from(info.nNumberOfLinks))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn a_file_counts_each_of_its_names() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("one.md");
        fs::write(&file, "").unwrap();

        assert_eq!(links(&file).unwrap(), 1);
        fs::hard_link(&file, dir.join("two.md")).unwrap();
        assert_eq!(links(&file).unwrap(), 2);
        links(&dir.join("missing.md")).unwrap_err();
    }
}
