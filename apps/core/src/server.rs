//! The core's way to the server: HTTPS requests and a WebSocket, both over TLS checked against
//! the public roots or certificates the person pinned, never skipped.

use std::sync::Arc;
use std::time::Duration;

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
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WireError, http::header::AUTHORIZATION};
use ureq::tls::{Certificate, RootCerts, TlsConfig};
use weacon_observability::Fault;
use weacon_protocol::{Code, ErrorBody, MAX_FRAME, code};

/// How long connecting, the TLS handshake, and each request may take.
const WITHIN: Duration = Duration::from_secs(20);
/// The largest answer read from the server.
const MAX_ANSWER: u64 = 64 * 1024;

/// A machine's open connection to the server.
pub(crate) type Socket = WebSocketStream<TlsStream<TcpStream>>;

/// A server the core reaches at `https://host[:port]`.
pub(crate) struct Server {
    host: String,
    port: u16,
    name: ServerName<'static>,
    tls: TlsConnector,
    agent: ureq::Agent,
}

/// Why a request didn't get the answer it asked for.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The server couldn't be reached, or broke off; worth trying again later.
    Unreachable(Fault),
    /// The server answered with this error status, and this code when its body gave one.
    Refused(u16, Option<Code>),
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
        let pins = pinned.map(pins).transpose()?;
        let mut roots = RootCertStore::empty();
        match &pins {
            Some(pins) => drop(roots.add_parsable_certificates(pins.iter().cloned())),
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let root_certs = pins.map_or(RootCerts::WebPki, |pins| {
            let pins = pins.iter().map(|pin| Certificate::from_der(pin).to_owned());
            RootCerts::Specific(Arc::new(pins.collect()))
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(WITHIN))
            .https_only(true)
            // A redirect would leave the checked server.
            .max_redirects(0)
            .http_status_as_error(false)
            .tls_config(TlsConfig::builder().root_certs(root_certs).build())
            .build()
            .into();
        Ok(Self {
            host,
            port,
            name,
            tls: TlsConnector::from(Arc::new(config)),
            agent,
        })
    }

    /// Sends `body` as JSON to `path`, and reads the JSON answer; it blocks, so it is for
    /// commands, not the daemon.
    pub(crate) fn post<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R, Failure> {
        let body = serde_json::to_vec(body).map_err(|error| unreachable().because(error))?;
        let sent = self
            .agent
            .post(format!("https://{}{path}", self.authority()))
            .header("Content-Type", "application/json")
            .send(body.as_slice());
        let mut response = sent.map_err(|error| unreachable().because(error))?;
        let answer = response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_to_vec()
            .map_err(|error| odd().because(error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Failure::Refused(status.as_u16(), code_in(&answer)));
        }
        serde_json::from_slice(&answer).map_err(|error| Failure::Unreachable(odd().because(error)))
    }

    /// Opens the WebSocket at `path`, proving the machine with `token`.
    pub(crate) async fn socket(&self, path: &str, token: &str) -> Result<Socket, Failure> {
        let mut request = format!("wss://{}{path}", self.authority())
            .into_client_request()
            .map_err(|error| unreachable().because(error))?;
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| Failure::Unreachable(unreachable()))?;
        request.headers_mut().insert(AUTHORIZATION, bearer);
        let stream = self.stream().await?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME));
        let opened = tokio_tungstenite::client_async_with_config(request, stream, Some(config));
        match timeout(WITHIN, opened).await {
            Ok(Ok((socket, _))) => Ok(socket),
            Ok(Err(WireError::Http(response))) => {
                let code = response.body().as_deref().and_then(code_in);
                Err(Failure::Refused(response.status().as_u16(), code))
            }
            Ok(Err(error)) => Err(Failure::Unreachable(unreachable().because(error))),
            Err(_) => Err(Failure::Unreachable(unreachable())),
        }
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

/// The error code in a refusal's body, if it has one.
fn code_in(body: &[u8]) -> Option<Code> {
    serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .map(|error| error.code)
}

/// The certificates in `pem`, at least one.
fn pins(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, Fault> {
    let pins = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| bad_pin())?;
    if pins.is_empty() {
        return Err(bad_pin());
    }
    Ok(pins)
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

    use weacon_protocol::ErrorBody;

    use super::*;
    use crate::test_support::tls::{Tls, answer, read_request};

    // Posts `body` to `path` off the runtime, as a command does, trusting `pinned`.
    async fn posted(
        tls: &Tls,
        pinned: Option<String>,
        path: &'static str,
        body: Value,
    ) -> Result<Value, Failure> {
        let url = tls.url();
        tokio::task::spawn_blocking(move || {
            let server = Server::new(&url, pinned.as_deref().map(str::as_bytes)).unwrap();
            server.post(path, &body)
        })
        .await
        .unwrap()
    }

    // An answer well past a few kilobytes, which is still allowed.
    fn long() -> Value {
        json!({ "ok": "x".repeat(32 * 1024) })
    }

    fn error_body(code: Code) -> String {
        serde_json::to_string(&ErrorBody {
            code,
            retryable: false,
        })
        .unwrap()
    }

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
        let serve = async {
            let mut stream = tls.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            answer(&mut stream, "201 Created", &long().to_string()).await;
            request
        };

        let body = json!({ "name": "Studio" });
        let (answered, (head, body)) = tokio::join!(
            posted(&tls, Some(tls.pem.clone()), "/api/machines/pair", body),
            serve
        );

        assert_eq!(answered.unwrap(), long());
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
        let serve = async {
            let mut stream = tls.accept().await.unwrap();
            read_request(&mut stream).await;
            answer(
                &mut stream,
                "404 Not Found",
                &error_body(code!("pairing.not_found")),
            )
            .await;
        };

        let (answered, ()) = tokio::join!(
            posted(&tls, Some(tls.pem.clone()), "/x", Value::Null),
            serve
        );

        assert!(
            matches!(&answered, Err(Failure::Refused(404, Some(code))) if *code == code!("pairing.not_found")),
            "{answered:?}"
        );
    }

    #[tokio::test]
    async fn a_certificate_neither_public_nor_pinned_is_refused() {
        let tls = Tls::new().await;
        let other = Tls::new().await;
        for pinned in [None, Some(other.pem.clone())] {
            let (answered, refused) =
                tokio::join!(posted(&tls, pinned, "/x", Value::Null), tls.accept());

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
                &error_body(code!("machine.unpaired")),
            )
            .await;
        };

        let (socket, ()) = tokio::join!(server.socket("/api/machines/connect", "t0ken"), serve);

        assert!(
            matches!(socket, Err(Failure::Refused(401, Some(code))) if code == code!("machine.unpaired"))
        );
    }
}
