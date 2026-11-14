//! A TLS server on this machine, with a certificate made for the test, that the core dials.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::server::TlsStream;

/// How long the server waits for the client, so a client that gave up fails the test.
const WAIT: Duration = Duration::from_secs(10);

/// A server for `localhost` and the PEM of its certificate, which a client pins.
pub(crate) struct Tls {
    pub(crate) address: SocketAddr,
    pub(crate) pem: String,
    listener: TcpListener,
    acceptor: TlsAcceptor,
}

impl Tls {
    pub(crate) async fn new() -> Self {
        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()));
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![made.cert.der().clone()], key)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self {
            address: listener.local_addr().unwrap(),
            pem: made.cert.pem(),
            listener,
            acceptor: TlsAcceptor::from(Arc::new(config)),
        }
    }

    /// The server's URL, by a name its certificate holds.
    pub(crate) fn url(&self) -> String {
        format!("https://localhost:{}", self.address.port())
    }

    /// The next connection after its handshake; `None` when the client gave up on it.
    pub(crate) async fn accept(&self) -> Option<TlsStream<TcpStream>> {
        let (tcp, _) = timeout(WAIT, self.listener.accept())
            .await
            .unwrap()
            .unwrap();
        timeout(WAIT, self.acceptor.accept(tcp)).await.unwrap().ok()
    }

    /// Closes each connection as it comes, until `done` is notified, so a client that asks
    /// more than it should fails at once.
    pub(crate) async fn refuse_until(&self, done: &Notify) {
        loop {
            tokio::select! {
                () = done.notified() => return,
                accepted = self.listener.accept() => drop(accepted),
            }
        }
    }
}

/// A URL on this machine where nothing listens.
pub(crate) fn nowhere() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("https://localhost:{port}")
}

/// Reads one HTTP request from `stream`: its head and body.
pub(crate) async fn read_request(stream: &mut TlsStream<TcpStream>) -> (String, String) {
    let mut seen = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = timeout(WAIT, stream.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(read, 0, "the client left before its request ended");
        seen.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&seen).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length: ")?
                        .parse()
                        .ok()
                })
                .unwrap_or(0);
            if body.len() >= length {
                return (head.to_owned(), body.to_owned());
            }
        }
    }
}

/// Answers with `status` and the JSON `body`.
pub(crate) async fn answer(stream: &mut TlsStream<TcpStream>, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}
