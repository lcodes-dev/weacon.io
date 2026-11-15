//! A TLS front for trying Weacon on one machine: the core only dials a server over TLS it can
//! verify, and the server speaks plain HTTP, so this answers TLS for `localhost` with a
//! certificate it made, which the core pins, and passes each connection on to the server.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::Failure;
use crate::platform::{create_private_dir, write_private};

/// The certificate's file in the front's folder, which the core pins.
pub const CERTIFICATE: &str = "cert.pem";
const KEY: &str = "key.pem";

/// The TLS setup for the front's certificate for `localhost`, from `dir`; the certificate and
/// its key are made there first, in a folder only this user can read, when it has none.
///
/// # Errors
/// When the folder can't be made or read, or holds a certificate without its key.
pub fn server_config(dir: &Path) -> Result<ServerConfig, Failure> {
    if !dir.join(CERTIFICATE).exists() {
        make(dir)?;
    }
    let cert = CertificateDer::from_pem_slice(&std::fs::read(dir.join(CERTIFICATE))?)?;
    let key = PrivateKeyDer::from_pem_slice(&std::fs::read(dir.join(KEY))?)?;
    Ok(ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?)
}

/// Writes the key first, so a certificate always has its key beside it.
fn make(dir: &Path) -> Result<(), Failure> {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    if let Err(error) = create_private_dir(dir)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error.into());
    }
    // A key left by a run that stopped before its certificate is replaced.
    if let Err(error) = std::fs::remove_file(dir.join(KEY))
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error.into());
    }
    write_private(&dir.join(KEY), made.signing_key.serialize_pem().as_bytes())?;
    std::fs::write(dir.join(CERTIFICATE), made.cert.pem())?;
    Ok(())
}

/// Answers TLS on each connection to `listener` and passes it on to `server`, until stopped.
///
/// # Errors
/// When the listener fails.
pub async fn front(
    listener: TcpListener,
    config: ServerConfig,
    server: SocketAddr,
) -> io::Result<()> {
    let acceptor = TlsAcceptor::from(Arc::new(config));
    loop {
        let (client, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        tokio::spawn(async move { pass_on(client, &acceptor, server).await });
    }
}

async fn pass_on(client: TcpStream, acceptor: &TlsAcceptor, server: SocketAddr) -> io::Result<()> {
    let mut client = acceptor.accept(client).await?;
    let mut server = TcpStream::connect(server).await?;
    tokio::io::copy_bidirectional(&mut client, &mut server).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TempDir;

    #[test]
    fn makes_its_certificate_once_in_a_folder_of_its_own() {
        let temp = TempDir::new().unwrap();
        let dir = temp.join("tls");
        server_config(&dir).unwrap();
        let first = std::fs::read(dir.join(CERTIFICATE)).unwrap();

        server_config(&dir).unwrap();

        assert_eq!(std::fs::read(dir.join(CERTIFICATE)).unwrap(), first);
    }

    #[test]
    fn a_folder_left_without_a_certificate_gets_one() {
        let temp = TempDir::new().unwrap();

        server_config(&temp).unwrap();

        assert!(temp.join(CERTIFICATE).exists());
    }

    #[test]
    fn a_key_left_without_its_certificate_is_replaced() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.join(KEY), "left").unwrap();

        server_config(&temp).unwrap();

        assert_ne!(std::fs::read_to_string(temp.join(KEY)).unwrap(), "left");
    }

    #[test]
    fn a_certificate_without_its_key_fails() {
        let temp = TempDir::new().unwrap();
        server_config(&temp).unwrap();
        std::fs::remove_file(temp.join(KEY)).unwrap();

        server_config(&temp).unwrap_err();
    }
}
