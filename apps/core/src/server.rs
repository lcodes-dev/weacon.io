//! The core's way to the server: HTTPS requests and a WebSocket, both over TLS checked against
//! the public roots or a certificate the person pinned, never skipped.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use weacon_observability::Fault;
use weacon_protocol::{MAX_FRAME, code};

/// How long connecting, the TLS handshake, and each request may take.
const WITHIN: Duration = Duration::from_secs(20);
/// The largest answer read from the server.
const MAX_ANSWER: usize = 64 * 1024;

/// A server the core reaches at `https://host[:port]`.
#[derive(Clone)]
pub(crate) struct Server {
    host: String,
    port: u16,
    name: ServerName<'static>,
    tls: TlsConnector,
}

/// Why a request didn't get the answer it asked for.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The server couldn't be reached, or broke off; worth trying again later.
    Unreachable(Fault),
    /// The server answered with an error status.
    Refused(StatusCode),
}

impl Server {
    /// The server at `url`, checked against `pinned`, a PEM file's certificates, or else the
    /// public roots. Only `https` URLs with no path are taken.
    ///
    /// # Errors
    /// When the URL isn't one, or `pinned` holds no certificate.
    pub(crate) fn new(url: &str, pinned: Option<&[u8]>) -> Result<Self, Fault> {
        let (host, port) = host_and_port(url).ok_or_else(bad_url)?;
        let name = ServerName::try_from(host.clone()).map_err(|_| bad_url())?;
        let mut roots = RootCertStore::empty();
        match pinned {
            Some(pem) => {
                let certificates = CertificateDer::pem_slice_iter(pem)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| bad_pin())?;
                let (added, _) = roots.add_parsable_certificates(certificates);
                if added == 0 {
                    return Err(bad_pin());
                }
            }
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            host,
            port,
            name,
            tls: TlsConnector::from(Arc::new(config)),
        })
    }

    /// Sends `body` as JSON to `path`, and reads the JSON answer.
    pub(crate) async fn post<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R, Failure> {
        let body = serde_json::to_vec(body).map_err(|error| unreachable().because(error))?;
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(HOST, self.authority())
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| unreachable().because(error))?;
        let (status, answer) = timeout(WITHIN, self.exchange(request))
            .await
            .map_err(|_| Failure::Unreachable(unreachable()))??;
        if status.is_success() {
            serde_json::from_slice(&answer)
                .map_err(|error| Failure::Unreachable(odd().because(error)))
        } else {
            Err(Failure::Refused(status))
        }
    }

    /// Opens the WebSocket at `path`, proving the machine with `token`.
    pub(crate) async fn socket(
        &self,
        path: &str,
        token: &str,
    ) -> Result<WebSocketStream<TlsStream<TcpStream>>, Failure> {
        let mut request = format!("wss://{}{path}", self.authority())
            .into_client_request()
            .map_err(|error| unreachable().because(error))?;
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| Failure::Unreachable(unreachable()))?;
        request.headers_mut().insert(AUTHORIZATION, bearer);
        let stream = self.stream().await?;
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME));
        let opened = tokio_tungstenite::client_async_with_config(request, stream, Some(config));
        match timeout(WITHIN, opened).await {
            Ok(Ok((socket, _))) => Ok(socket),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
                Err(Failure::Refused(response.status()))
            }
            Ok(Err(error)) => Err(Failure::Unreachable(unreachable().because(error))),
            Err(_) => Err(Failure::Unreachable(unreachable())),
        }
    }

    async fn exchange(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<(StatusCode, Bytes), Failure> {
        let stream = self.stream().await?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|error| unreachable().because(error))?;
        let connection = tokio::spawn(connection);
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| unreachable().because(error))?;
        let status = response.status();
        let answer = Limited::new(response.into_body(), MAX_ANSWER)
            .collect()
            .await
            .map_err(|_| Failure::Unreachable(odd()))?
            .to_bytes();
        connection.abort();
        Ok((status, answer))
    }

    async fn stream(&self) -> Result<TlsStream<TcpStream>, Failure> {
        let connect = async {
            let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
            self.tls.connect(self.name.clone(), tcp).await
        };
        match timeout(WITHIN, connect).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(error)) => Err(Failure::Unreachable(unreachable().because(error))),
            Err(_) => Err(Failure::Unreachable(unreachable())),
        }
    }

    fn authority(&self) -> String {
        if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl From<Fault> for Failure {
    fn from(fault: Fault) -> Self {
        Self::Unreachable(fault)
    }
}

/// The host and port of an `https://host[:port]` URL, a trailing slash allowed.
fn host_and_port(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("https://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let fine = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':');
    if rest.is_empty() || !rest.bytes().all(fine) {
        return None;
    }
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.contains(':') => {
            Some((host.to_ascii_lowercase(), port.parse().ok()?))
        }
        Some(_) => None,
        None => Some((rest.to_ascii_lowercase(), 443)),
    }
}

fn bad_url() -> Fault {
    Fault::new(
        code!("server.url_invalid"),
        "The server's address must be an https:// URL with no path",
    )
}

fn bad_pin() -> Fault {
    Fault::new(
        code!("server.pin_invalid"),
        "The pinned certificate file holds no certificate",
    )
}

fn unreachable() -> Fault {
    Fault::new(
        code!("server.unreachable"),
        "The server couldn't be reached",
    )
}

fn odd() -> Fault {
    Fault::new(
        code!("server.answer_invalid"),
        "The server's answer couldn't be read",
    )
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::Message as Wire;

    use super::*;
    use crate::test_support::tls::{Tls, answer, read_request};

    #[test]
    fn only_an_https_url_with_no_path_names_a_server() {
        let cases = [
            (
                "https://weacon.example.com",
                Some(("weacon.example.com", 443)),
            ),
            (
                "https://Weacon.Example.com:8443/",
                Some(("weacon.example.com", 8443)),
            ),
            ("https://10.0.0.2:9000", Some(("10.0.0.2", 9000))),
            ("http://weacon.example.com", None),
            ("wss://weacon.example.com", None),
            ("https://weacon.example.com/api", None),
            ("https://user@weacon.example.com", None),
            ("https://", None),
            ("https://:443", None),
            ("https://host:99999", None),
            ("https://[::1]:443", None),
        ];
        for (url, expected) in cases {
            let found = host_and_port(url);
            let expected = expected.map(|(host, port)| (host.to_owned(), port));
            assert_eq!(found, expected, "{url}");
        }
        assert_eq!(
            Server::new("http://weacon.example.com", None)
                .err()
                .map(|fault| fault.code().clone()),
            Some(code!("server.url_invalid"))
        );
        assert_eq!(
            Server::new("https://weacon.example.com", Some(b"not a pem"))
                .err()
                .map(|fault| fault.code().clone()),
            Some(code!("server.pin_invalid"))
        );
    }

    #[tokio::test]
    async fn a_request_reaches_a_server_whose_certificate_is_pinned() {
        let tls = Tls::new().await;
        let server = Server::new(&tls.url(), Some(tls.pem.as_bytes())).unwrap();
        let serve = async {
            let mut stream = tls.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            answer(&mut stream, "201 Created", r#"{"ok":true}"#).await;
            request
        };

        let body = json!({ "name": "Studio" });
        let (answered, (head, body)) =
            tokio::join!(server.post::<_, Value>("/api/machines/pair", &body), serve);

        assert_eq!(answered.unwrap(), json!({ "ok": true }));
        assert!(
            head.starts_with("POST /api/machines/pair HTTP/1.1"),
            "{head}"
        );
        assert!(
            head.to_lowercase()
                .contains(&format!("host: localhost:{}", tls.address.port()))
        );
        assert_eq!(body, r#"{"name":"Studio"}"#);
    }

    #[tokio::test]
    async fn a_refusal_gives_its_status() {
        let tls = Tls::new().await;
        let server = Server::new(&tls.url(), Some(tls.pem.as_bytes())).unwrap();
        let serve = async {
            let mut stream = tls.accept().await.unwrap();
            read_request(&mut stream).await;
            answer(
                &mut stream,
                "404 Not Found",
                r#"{"code":"pairing.not_found","retryable":false}"#,
            )
            .await;
        };

        let (answered, ()) = tokio::join!(server.post::<_, Value>("/x", &Value::Null), serve);

        assert!(
            matches!(answered, Err(Failure::Refused(StatusCode::NOT_FOUND))),
            "{answered:?}"
        );
    }

    #[tokio::test]
    async fn a_certificate_neither_public_nor_pinned_is_refused() {
        let tls = Tls::new().await;
        let other = Tls::new().await;
        for pinned in [None, Some(other.pem.as_bytes())] {
            let server = Server::new(&tls.url(), pinned).unwrap();
            let (answered, refused) =
                tokio::join!(server.post::<_, Value>("/x", &Value::Null), tls.accept());

            assert!(
                matches!(answered, Err(Failure::Unreachable(_))),
                "{answered:?}"
            );
            assert!(refused.is_none());
        }
    }

    #[tokio::test]
    async fn a_socket_opens_over_the_same_checked_tls() {
        let tls = Tls::new().await;
        let server = Server::new(&tls.url(), Some(tls.pem.as_bytes())).unwrap();
        let serve = async {
            let stream = tls.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.send(Wire::text("hi")).await.unwrap();
        };

        let (socket, ()) = tokio::join!(server.socket("/api/machines/connect", "t0ken"), serve);

        let mut socket = socket.unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap(), Wire::text("hi"));
    }

    #[tokio::test]
    async fn a_socket_the_server_refuses_gives_its_status() {
        let tls = Tls::new().await;
        let server = Server::new(&tls.url(), Some(tls.pem.as_bytes())).unwrap();
        let serve = async {
            let mut stream = tls.accept().await.unwrap();
            read_request(&mut stream).await;
            answer(
                &mut stream,
                "401 Unauthorized",
                r#"{"code":"machine.unpaired","retryable":false}"#,
            )
            .await;
        };

        let (socket, ()) = tokio::join!(server.socket("/api/machines/connect", "t0ken"), serve);

        assert!(matches!(
            socket,
            Err(Failure::Refused(StatusCode::UNAUTHORIZED))
        ));
    }
}
