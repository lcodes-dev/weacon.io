//! A TLS front for trying Weacon on one machine: the core only dials a server over TLS it can
//! verify, and the server speaks plain HTTP, so this answers TLS for `localhost` with a
//! certificate it made, which the core pins, and passes each connection on to the server.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::Failure;
use crate::platform::create_private_dir;

/// The certificate's file in the front's folder, which the core pins.
pub const CERTIFICATE: &str = "cert.pem";
const KEY: &str = "key.pem";

/// The front's certificate for `localhost` and its key, from `dir`; made there first, in a
/// folder only this user can read, when `dir` doesn't exist.
///
/// # Errors
/// When the folder can't be made or read, or holds no certificate and key.
pub fn certificate(dir: &Path) -> Result<ServerConfig, Failure> {
    if !dir.exists() {
        make(dir)?;
    }
    let cert = CertificateDer::from_pem_slice(&std::fs::read(dir.join(CERTIFICATE))?)?;
    let key = PrivateKeyDer::from_pem_slice(&std::fs::read(dir.join(KEY))?)?;
    Ok(ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?)
}

fn make(dir: &Path) -> Result<(), Failure> {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    // Made beside its place and renamed into it, so a failure leaves no half-made folder.
    let mut building = dir.as_os_str().to_owned();
    building.push(".new");
    let building = PathBuf::from(building);
    create_private_dir(&building)?;
    std::fs::write(building.join(KEY), made.signing_key.serialize_pem())?;
    std::fs::write(building.join(CERTIFICATE), made.cert.pem())?;
    std::fs::rename(&building, dir)?;
    Ok(())
}

/// Listens on `port` of 127.0.0.1.
///
/// # Errors
/// When the port is taken.
pub async fn listen(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::pki_types::ServerName;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};

    use super::*;
    use crate::TempDir;

    fn pinned(dir: &Path) -> TlsConnector {
        let pem = std::fs::read(dir.join(CERTIFICATE)).unwrap();
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(&pem).unwrap())
            .unwrap();
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        TlsConnector::from(Arc::new(config))
    }

    #[tokio::test]
    async fn passes_a_connection_on_over_tls_the_pinned_certificate_verifies() {
        let temp = TempDir::new().unwrap();
        let dir = temp.join("tls");
        let config = certificate(&dir).unwrap();
        let first = std::fs::read(dir.join(CERTIFICATE)).unwrap();
        certificate(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(CERTIFICATE)).unwrap(), first);
        assert!(!temp.join("tls.new").exists());

        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = server.local_addr().unwrap();
        let echo = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut asked = [0; 5];
            stream.read_exact(&mut asked).await.unwrap();
            stream.write_all(&asked).await.unwrap();
        });
        let listener = listen(0).await.unwrap();
        let address = listener.local_addr().unwrap();
        assert!(address.ip().is_loopback());
        tokio::spawn(front(listener, config, upstream));

        let tcp = TcpStream::connect(address).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let mut tls = pinned(&dir).connect(name, tcp).await.unwrap();
        tls.write_all(b"hello").await.unwrap();
        let mut answer = [0; 5];
        tls.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"hello");
        echo.await.unwrap();
    }

    #[test]
    fn a_folder_without_a_certificate_fails() {
        let temp = TempDir::new().unwrap();

        certificate(&temp).unwrap_err();
    }
}
