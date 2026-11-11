use std::fs::Metadata;

/// Whether only its owner can read or write the file `metadata` is of; always, where the
/// system has no such permissions.
#[cfg_attr(
    unix,
    expect(
        clippy::verbose_bit_mask,
        reason = "the mask is how permissions are written"
    )
)]
pub(crate) fn private(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;

    use weacon_testing::TempDir;

    use super::*;
    use crate::secret::Secret;

    #[test]
    fn a_file_others_can_read_or_write_is_not_private() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("file");
        fs::write(&file, "").unwrap();
        let private_with = |mode| {
            fs::set_permissions(&file, Permissions::from_mode(mode)).unwrap();
            private(&fs::metadata(&file).unwrap())
        };

        assert!(private_with(0o600));
        assert!(private_with(0o400));
        assert!(!private_with(0o640));
        assert!(!private_with(0o604));
        assert!(!private_with(0o602));
    }

    #[test]
    fn a_secret_others_can_read_is_refused() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("secret");
        fs::write(&file, [1; 32]).unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o644)).unwrap();

        let exposed = Secret::open(&file).unwrap_err();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();

        assert_eq!(exposed.code().as_str(), "server.secret_exposed");
        Secret::open(&file).unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o400)).unwrap();
        Secret::open(&file).unwrap();
    }
}
