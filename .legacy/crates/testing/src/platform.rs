//! How test folders and programs are made, per OS.

#[cfg(unix)]
pub(crate) use unix::create_private_dir;
#[cfg(unix)]
pub use unix::write_program;
#[cfg(windows)]
pub(crate) use windows::create_private_dir;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::path::Path;

    /// Creates the folder `path`, readable only by this user.
    pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new().mode(0o700).create(path)
    }

    /// Writes an executable `script` to `path` from a child shell, so this process never holds a
    /// write handle that another test's fork could inherit, which makes running it fail as busy.
    ///
    /// # Errors
    /// When the shell can't be run or can't write the program.
    pub fn write_program(path: &Path, script: &str) -> io::Result<()> {
        let written = std::process::Command::new("sh")
            .args([
                "-c",
                "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
                "sh",
                script,
            ])
            .arg(path)
            .status()?;
        if written.success() {
            Ok(())
        } else {
            Err(io::Error::other("the program couldn't be written"))
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;

    /// Creates the folder `path`, which inherits its parent's access list.
    pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn writes_a_program_anyone_may_run_and_says_when_it_cant() {
        let dir = crate::TempDir::new().unwrap();
        let path = dir.join("hello");

        write_program(&path, "#!/bin/sh\necho hi\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "#!/bin/sh\necho hi\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        write_program(&dir.join("missing/hello"), "").unwrap_err();
    }
}
