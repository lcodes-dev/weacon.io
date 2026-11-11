use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use weacon_observability::Fault;
use weacon_protocol::{ChallengeId, code};

use crate::app::new_id;
use crate::platform::{private, sync_folder};

/// The server's own key, which sign-in codes are hashed with: a stolen database alone can't
/// be searched for its eight-digit codes.
pub(crate) struct Secret(Hmac<Sha256>);

impl Secret {
    /// Reads the 32-byte key in the file at `path`, which only this user may read, or makes
    /// one there. It blocks.
    pub(crate) fn open(path: &Path) -> Result<Self, Fault> {
        if let Ok(file) = File::open(path) {
            return Self::read(file);
        }
        create(path)?;
        Self::read(File::open(path).map_err(unreadable)?)
    }

    fn read(mut file: File) -> Result<Self, Fault> {
        let metadata = file.metadata().map_err(unreadable)?;
        if !private(&metadata) {
            return Err(Fault::new(
                code!("server.secret_exposed"),
                "The secret file can be read by other users",
            ));
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

    /// What is stored for the code of `challenge`, which is part of it so that the same code
    /// in two challenges is stored differently.
    pub(crate) fn code_hash(&self, challenge: &ChallengeId, code: &str) -> [u8; 32] {
        let mut mac = self.0.clone();
        mac.update(challenge.as_str().as_bytes());
        // A challenge id never holds a zero byte, so this keeps the two apart.
        mac.update(&[0]);
        mac.update(code.as_bytes());
        mac.finalize().into_bytes().into()
    }
}

/// Makes a key at `path`, whole or not at all; a file already there is left as it is.
fn create(path: &Path) -> Result<(), Fault> {
    let key = os_secrets()?;
    let mut new = OsString::from(path);
    new.push(format!(".{}.new", new_id()));
    let new = PathBuf::from(new);
    let made = weacon_appkit::private_append(&new).and_then(|mut file| {
        file.write_all(&key)?;
        file.sync_all()
    });
    if made.is_ok() {
        // Fails when another server made the file first; then its key is the one read.
        drop(fs::hard_link(&new, path));
    }
    drop(fs::remove_file(&new));
    // The folder holds the key's name: without this, a crash could leave no key behind.
    made.and_then(|()| sync_folder(folder_of(path)))
        .map_err(unreadable)
}

/// The folder `path` is in; the current one for a bare file name.
fn folder_of(path: &Path) -> &Path {
    path.parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
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
    use weacon_testing::TempDir;

    use super::*;

    fn challenge(id: &str) -> ChallengeId {
        ChallengeId::try_from(id.to_owned()).unwrap()
    }

    fn hash(secret: &Secret) -> [u8; 32] {
        secret.code_hash(&challenge("c1"), "12345678")
    }

    // A private file holding `bytes`.
    fn stored(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        weacon_appkit::private_append(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
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
        let first = Secret::open(&dir.join("secret")).unwrap();
        let again = Secret::open(&dir.join("secret")).unwrap();
        let other = Secret::open(&dir.join("other")).unwrap();
        let metadata = fs::metadata(dir.join("secret")).unwrap();

        assert_eq!(metadata.len(), 32);
        assert!(private(&metadata));
        assert_eq!(hash(&first), hash(&again));
        assert_ne!(hash(&first), hash(&other));
        assert_eq!(names(&dir), ["other", "secret"]);
        assert_eq!(format!("{first:?}"), "Secret(..)");
    }

    #[test]
    fn a_secret_made_meanwhile_is_kept() {
        let dir = TempDir::new().unwrap();
        let path = stored(&dir, "secret", &[7; 32]);
        let theirs = hash(&Secret::open(&path).unwrap());

        create(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), [7; 32]);
        assert_eq!(hash(&Secret::open(&path).unwrap()), theirs);
        assert_eq!(names(&dir), ["secret"]);
    }

    #[test]
    fn a_secret_of_another_length_or_out_of_reach_is_refused() {
        let dir = TempDir::new().unwrap();
        let code_of = |path: &Path| Secret::open(path).unwrap_err().code().as_str().to_owned();

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
    fn a_code_is_hashed_with_the_key_and_its_challenge() {
        let dir = TempDir::new().unwrap();
        let key: Vec<u8> = (0..32).collect();
        let secret = Secret::open(&stored(&dir, "secret", &key)).unwrap();
        let other = Secret::open(&dir.join("other")).unwrap();

        // HMAC-SHA256 of "c1", a zero byte and the code, under the bytes 0 to 31.
        assert_eq!(
            hash(&secret).map(|byte| format!("{byte:02x}")).concat(),
            "de17a9792373011b788c5cdb054da494710e8160ee9b5405380cd0f499bca7ba"
        );
        assert_ne!(
            hash(&secret),
            secret.code_hash(&challenge("c2"), "12345678")
        );
        assert_ne!(
            hash(&secret),
            secret.code_hash(&challenge("c1"), "12345679")
        );
        assert_ne!(hash(&secret), hash(&other));
    }

    #[test]
    fn a_bare_file_name_is_in_the_current_folder() {
        assert_eq!(folder_of(Path::new("secret")), Path::new("."));
        assert_eq!(folder_of(Path::new("keys/secret")), Path::new("keys"));
        assert_eq!(folder_of(Path::new("/secret")), Path::new("/"));
    }

    #[test]
    fn random_bytes_differ() {
        assert_ne!(os_secrets().unwrap(), os_secrets().unwrap());
    }
}
