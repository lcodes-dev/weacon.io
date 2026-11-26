//! The server's key, kept outside the database, which seals the secrets the database holds.

use std::io::ErrorKind;
use std::path::Path;

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sqlx::SqlitePool;

use crate::db;

#[derive(Clone)]
pub struct ServerKey([u8; 32]);

impl ServerKey {
    pub fn random() -> Self {
        ServerKey(rand::random())
    }

    /// `place` names where the value is kept, so that it opens nowhere else.
    pub fn seal(&self, place: &str, value: &str) -> String {
        let nonce: [u8; NONCE_LEN] = rand::random();
        let mut sealed = value.as_bytes().to_vec();
        self.cipher()
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(place),
                &mut sealed,
            )
            .expect("AES-GCM seals any value under 64 GB");
        STANDARD.encode([&nonce[..], &sealed].concat())
    }

    pub fn open(&self, place: &str, sealed: &str) -> Option<String> {
        let bytes = STANDARD.decode(sealed).ok()?;
        let (nonce, sealed) = bytes.split_at_checked(NONCE_LEN)?;
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut sealed = sealed.to_vec();
        let value = self
            .cipher()
            .open_in_place(nonce, Aad::from(place), &mut sealed)
            .ok()?;
        String::from_utf8(value.to_vec()).ok()
    }

    fn cipher(&self) -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &self.0).expect("the key is 32 bytes"))
    }
}

/// Reads the key from `path`, or makes it there while no sealed secret needs the old one;
/// then seals any secret still plain, and checks that the key opens them all.
pub async fn load(
    pool: &SqlitePool,
    path: &Path,
) -> Result<ServerKey, Box<dyn std::error::Error + Send + Sync>> {
    let key_file = path.display();
    let key = match std::fs::read(path) {
        Ok(bytes) => ServerKey(
            bytes
                .try_into()
                .map_err(|_| format!("{key_file} doesn't hold a key of 32 bytes"))?,
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if db::settings::secrets_sealed(pool).await?
                && !db::settings::secrets(pool).await?.is_empty()
            {
                return Err(format!(
                    "{key_file} is missing, and the database's secrets were sealed with the key it held"
                )
                .into());
            }
            let key = ServerKey::random();
            write_private(path, &key.0).map_err(|error| format!("{key_file}: {error}"))?;
            key
        }
        Err(error) => return Err(format!("{key_file}: {error}").into()),
    };
    db::settings::seal_secrets(pool, &key).await?;
    for (place, sealed) in db::settings::secrets(pool).await? {
        if key.open(&place, &sealed).is_none() {
            return Err(
                format!("the key in {key_file} doesn't open the database's secrets").into(),
            );
        }
    }
    Ok(key)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut create = std::fs::OpenOptions::new();
    create.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut create, 0o600);
    create.open(path)?.write_all(bytes)
}
