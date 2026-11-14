use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use weacon_appkit::{create_private, is_private, sync_dir};
use weacon_observability::{Fault, Logger};
use weacon_protocol::code;

use crate::email::Email;

/// The server's own key, which sign-in codes are hashed with: a stolen database alone can't
/// be searched for its eight-digit codes.
pub(crate) struct Secret(Hmac<Sha256>);

impl Secret {
    /// Reads the 32-byte key in the file at `path`, which only this user may read, or makes
    /// one there; warns where the system can't tell who may read it. It blocks.
    pub(crate) fn open(path: &Path, log: &Logger) -> Result<Self, Fault> {
        let file = if let Ok(file) = File::open(path) {
            file
        } else {
            create(path)?;
            File::open(path).map_err(unreadable)?
        };
        // Even a key found there may be one another server has only just made: until its
        // folder is synced, a crash could lose it and every code hashed with it.
        path.parent().map_or(Ok(()), sync_dir).map_err(unreadable)?;
        Self::read(file, log)
    }

    fn read(mut file: File, log: &Logger) -> Result<Self, Fault> {
        let metadata = file.metadata().map_err(unreadable)?;
        match is_private(&metadata) {
            Some(true) => {}
            Some(false) => {
                return Err(Fault::new(
                    code!("server.secret_exposed"),
                    "The secret file can be read by other users",
                ));
            }
            None => log.warn("secret unchecked", &[]),
        }
        if metadata.len() != 32 {
            return Err(Fault::new(
                code!("server.secret_invalid"),
                "The secret file doesn't hold a 32-byte key",
            ));
        }
        let mut key = [0; 32];
        file.read_exact(&mut key).map_err(unreadable)?;
        // HMAC fills a short key with zeros up to its 64-byte block; filled here, no length
        // can be refused.
        let mut block = [0; 64];
        for (to, from) in block.iter_mut().zip(key) {
            *to = from;
        }
        Ok(Self(Hmac::new(&block.into())))
    }

    /// What is stored for a code mailed to `email`, which is part of it so that the same code
    /// for two addresses is stored differently; any of the address's challenges can check it.
    pub(crate) fn code_hash(&self, email: &Email, code: &str) -> [u8; 32] {
        let mut mac = self.0.clone();
        mac.update(email.as_str().as_bytes());
        // An address never holds a zero byte, so this keeps the two apart.
        mac.update(&[0]);
        mac.update(code.as_bytes());
        mac.finalize().into_bytes().into()
    }
}

/// Makes a key at `path`, whole or not at all; a file already there is left as it is.
fn create(path: &Path) -> Result<(), Fault> {
    let key = os_secrets()?;
    match create_private(path, &key) {
        // Another server made the file first; then its key is the one read.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        made => made.map_err(unreadable),
    }
}

/// The operating system's random bytes.
pub(crate) fn os_secrets() -> Result<[u8; 32], Fault> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| random_failed())?;
    Ok(bytes)
}

pub(crate) fn random_failed() -> Fault {
    Fault::new(
        code!("random.failed"),
        "The server couldn't get random bytes",
    )
}

fn unreadable(error: io::Error) -> Fault {
    Fault::new(
        code!("server.secret_unreadable"),
        "The server couldn't read or create its secret file",
    )
    .because(error)
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    use weacon_appkit::private_append;
    use weacon_testing::{TempDir, messages, recording_logger};

    use super::*;

    fn open(path: &Path) -> Result<Secret, Fault> {
        Secret::open(path, &recording_logger().0)
    }

    fn address(text: &str) -> Email {
        Email::parse(text).unwrap()
    }

    fn hash(secret: &Secret) -> [u8; 32] {
        secret.code_hash(&address("ada@example.com"), "12345678")
    }

    // A private file holding `bytes`.
    fn stored(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        private_append(&path).unwrap().write_all(bytes).unwrap();
        path
    }

    fn names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(&**dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_missing_secret_is_made_once_and_read_after() {
        let dir = TempDir::new().unwrap();
        let first = open(&dir.join("secret")).unwrap();
        let again = open(&dir.join("secret")).unwrap();
        let other = open(&dir.join("other")).unwrap();
        let metadata = fs::metadata(dir.join("secret")).unwrap();

        assert_eq!(metadata.len(), 32);
        assert_ne!(is_private(&metadata), Some(false));
        assert_eq!(hash(&first), hash(&again));
        assert_ne!(hash(&first), hash(&other));
        assert_eq!(names(&dir), ["other", "secret"]);
        assert_eq!(format!("{first:?}"), "Secret(..)");
    }

    #[test]
    fn a_secret_made_meanwhile_is_kept() {
        let dir = TempDir::new().unwrap();
        let path = stored(&dir, "secret", &[7; 32]);
        let theirs = hash(&open(&path).unwrap());

        create(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), [7; 32]);
        assert_eq!(hash(&open(&path).unwrap()), theirs);
        assert_eq!(names(&dir), ["secret"]);
    }

    #[test]
    fn a_secret_of_another_length_or_out_of_reach_is_refused() {
        let dir = TempDir::new().unwrap();
        let code_of = |path: &Path| open(path).unwrap_err().code().as_str().to_owned();

        assert_eq!(
            code_of(&stored(&dir, "short", &[1; 31])),
            "server.secret_invalid"
        );
        assert_eq!(
            code_of(&stored(&dir, "long", &[1; 33])),
            "server.secret_invalid"
        );
        assert_eq!(
            code_of(&stored(&dir, "empty", &[])),
            "server.secret_invalid"
        );
        assert_eq!(
            code_of(&dir.join("nowhere").join("secret")),
            "server.secret_unreadable"
        );
    }

    #[test]
    fn a_code_is_hashed_with_the_key_and_its_address() {
        let dir = TempDir::new().unwrap();
        let key: Vec<u8> = (0..32).collect();
        let secret = open(&stored(&dir, "secret", &key)).unwrap();
        let other = open(&dir.join("other")).unwrap();

        // HMAC-SHA256 of the address, a zero byte and the code, under the bytes 0 to 31.
        assert_eq!(
            hash(&secret).map(|byte| format!("{byte:02x}")).concat(),
            "a9a3d81fa081739162c1e32f8416dc4784edca22cb7a5b756b458b6cd03815e8"
        );
        assert_ne!(
            hash(&secret),
            secret.code_hash(&address("bo@example.com"), "12345678")
        );
        assert_ne!(
            hash(&secret),
            secret.code_hash(&address("ada@example.com"), "12345679")
        );
        assert_ne!(hash(&secret), hash(&other));
    }

    #[test]
    fn random_bytes_differ() {
        assert_ne!(os_secrets().unwrap(), os_secrets().unwrap());
    }

    #[test]
    fn a_secret_is_unchecked_only_where_permissions_cant_tell() {
        let dir = TempDir::new().unwrap();
        let (log, records) = recording_logger();
        Secret::open(&dir.join("secret"), &log).unwrap();
        let unknown = is_private(&fs::metadata(dir.join("secret")).unwrap()).is_none();

        assert_eq!(messages(&records).contains(&"secret unchecked"), unknown);
    }

    #[cfg(unix)]
    #[test]
    fn a_secret_others_can_read_is_refused() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let file = dir.join("secret");
        fs::write(&file, [1; 32]).unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o644)).unwrap();

        let exposed = open(&file).unwrap_err();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();

        assert_eq!(exposed.code().as_str(), "server.secret_exposed");
        open(&file).unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o400)).unwrap();
        open(&file).unwrap();
    }
}
