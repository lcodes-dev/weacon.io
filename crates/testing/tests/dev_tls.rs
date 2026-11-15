//! Runs the built `dev-tls` binary.

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::sync::Arc;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};
    use weacon_testing::TempDir;
    use weacon_testing::dev_tls::CERTIFICATE;

    fn dev_tls(args: &[&str]) -> Option<i32> {
        Command::new(env!("CARGO_BIN_EXE_dev-tls"))
            .args(args)
            .status()
            .unwrap()
            .code()
    }

    #[test]
    fn without_a_folder_or_with_a_bad_port_it_says_how_to_run_it() {
        assert_eq!(dev_tls(&[]), Some(2));
        assert_eq!(dev_tls(&["tls", "8443x"]), Some(2));
        assert_eq!(dev_tls(&["tls", "0", "70000"]), Some(2));
    }

    #[test]
    fn a_folder_it_cant_make_is_a_failure_it_names() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.join("file"), "").unwrap();
        let ran = Command::new(env!("CARGO_BIN_EXE_dev-tls"))
            .arg(temp.join("file/tls"))
            .output()
            .unwrap();

        assert_eq!(ran.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(ran.stderr).unwrap(),
            "dev-tls: its certificate folder can't be used\n"
        );
    }

    fn pinned(pem: &[u8]) -> TlsConnector {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(pem).unwrap())
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
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port().to_string();
        let mut front = tokio::process::Command::new(env!("CARGO_BIN_EXE_dev-tls"))
            .args([dir.as_os_str(), "0".as_ref(), port.as_ref()])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(front.stdout.take().unwrap()).lines();
        let url = lines.next_line().await.unwrap().unwrap();
        let address = url.strip_prefix("https://localhost:").unwrap();

        let tcp = TcpStream::connect(format!("127.0.0.1:{address}"))
            .await
            .unwrap();
        let pem = std::fs::read(dir.join(CERTIFICATE)).unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let mut tls = pinned(&pem).connect(name, tcp).await.unwrap();
        tls.write_all(b"hello").await.unwrap();
        let (mut passed, _) = server.accept().await.unwrap();
        let mut asked = [0; 5];
        passed.read_exact(&mut asked).await.unwrap();
        passed.write_all(b"world").await.unwrap();
        let mut answer = [0; 5];
        tls.read_exact(&mut answer).await.unwrap();

        assert_eq!(&asked, b"hello");
        assert_eq!(&answer, b"world");
    }
}
