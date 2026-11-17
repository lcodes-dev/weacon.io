//! The core's way to the server: HTTP requests and a WebSocket, over TLS checked against the
//! public roots, never skipped, or plain to a server on 127.0.0.1.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WireError, http::header::AUTHORIZATION};
use weacon_observability::Fault;
use weacon_protocol::{Code, ErrorBody, MAX_FRAME, code};

/// How long connecting, the TLS handshake, and each request may take.
const WITHIN: Duration = Duration::from_secs(20);
/// The largest answer read from the server.
const MAX_ANSWER: u64 = 64 * 1024;

/// The bytes under a connection: TLS, or plain to this machine.
pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<S: AsyncRead + AsyncWrite + Unpin + Send> Stream for S {}

/// A machine's open connection to the server.
pub(crate) type Socket = WebSocketStream<Box<dyn Stream>>;

/// A server the core reaches at `https://host[:port]` or `http://127.0.0.1[:port]`.
pub(crate) struct Server {
    host: String,
    port: u16,
    /// What checks the server's certificate; none for a server on this machine.
    tls: Option<(ServerName<'static>, TlsConnector)>,
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
    /// The server at `url`: an `https` URL, or an `http` one to 127.0.0.1, with no path.
    ///
    /// # Errors
    /// When the URL isn't one.
    pub(crate) fn new(url: &str) -> Result<Self, Fault> {
        let (secure, host, port) = address(url).ok_or_else(bad_url)?;
        let tls = if secure {
            let name = ServerName::try_from(host.clone()).map_err(|_| bad_url())?;
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            Some((name, TlsConnector::from(Arc::new(config))))
        } else {
            None
        };
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(WITHIN))
            .https_only(secure)
            // As the socket, which dials the server itself: no proxy from the environment.
            .proxy(None)
            // A redirect would leave the checked server.
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            host,
            port,
            tls,
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
        let scheme = if self.tls.is_some() { "https" } else { "http" };
        let sent = self
            .agent
            .post(format!("{scheme}://{}{path}", self.authority()))
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
        let scheme = if self.tls.is_some() { "wss" } else { "ws" };
        let mut request = format!("{scheme}://{}{path}", self.authority())
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

    async fn stream(&self) -> Result<Box<dyn Stream>, Failure> {
        let connect = async {
            let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
            let stream: Box<dyn Stream> = match &self.tls {
                Some((name, tls)) => Box::new(tls.connect(name.clone(), tcp).await?),
                None => Box::new(tcp),
            };
            std::io::Result::Ok(stream)
        };
        match timeout(WITHIN, connect).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(error)) => Err(Failure::Unreachable(unreachable().because(error))),
            Err(_) => Err(Failure::Unreachable(unreachable())),
        }
    }

    fn authority(&self) -> String {
        if self.tls.is_some() && self.port == 443 {
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

/// Whether it is `https`, and the host and port, of an `https://host[:port]` or
/// `http://127.0.0.1[:port]` URL, a trailing slash allowed.
fn address(url: &str) -> Option<(bool, String, u16)> {
    let (secure, rest) = match url.strip_prefix("https://") {
        Some(rest) => (true, rest),
        None => (false, url.strip_prefix("http://")?),
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let fine = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':');
    if rest.is_empty() || !rest.bytes().all(fine) {
        return None;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.contains(':') => (host, port.parse().ok()?),
        Some(_) => return None,
        None => (rest, if secure { 443 } else { 80 }),
    };
    (secure || host == "127.0.0.1").then(|| (secure, host.to_ascii_lowercase(), port))
}

fn bad_url() -> Fault {
    Fault::new(
        code!("server.url_invalid"),
        "The server's address must be an https:// URL, or http://127.0.0.1, with no path",
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
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_tungstenite::tungstenite::Message as Wire;

    use weacon_protocol::ErrorBody;

    use super::*;
    use crate::test_support::local::{Local, answer, read_request};

    // Posts `body` to `path` at `url` off the runtime, as a command does.
    async fn posted(url: String, path: &'static str, body: Value) -> Result<Value, Failure> {
        tokio::task::spawn_blocking(move || Server::new(&url).unwrap().post(path, &body))
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
    fn only_an_https_url_or_a_plain_one_to_this_machine_with_no_path_names_a_server() {
        let cases = [
            (
                "https://weacon.example.com",
                Some((true, "weacon.example.com", 443)),
            ),
            (
                "https://Weacon.Example.com:8443/",
                Some((true, "weacon.example.com", 8443)),
            ),
            ("https://10.0.0.2:9000", Some((true, "10.0.0.2", 9000))),
            ("http://127.0.0.1:8080/", Some((false, "127.0.0.1", 8080))),
            ("http://127.0.0.1", Some((false, "127.0.0.1", 80))),
            ("http://weacon.example.com", None),
            ("http://localhost:8080", None),
            ("http://127.0.0.2:8080", None),
            ("http://10.0.0.2:8080", None),
            ("http://127.0.0.1.example.com", None),
            ("wss://weacon.example.com", None),
            ("https://weacon.example.com/api", None),
            ("http://127.0.0.1:8080/api", None),
            ("https://user@weacon.example.com", None),
            ("http://user@127.0.0.1", None),
            ("https://", None),
            ("https://:443", None),
            ("https://host:99999", None),
            ("https://[::1]:443", None),
        ];
        for (url, expected) in cases {
            let found = address(url);
            let expected = expected.map(|(secure, host, port)| (secure, host.to_owned(), port));
            assert_eq!(found, expected, "{url}");
        }
        assert_eq!(
            Server::new("http://weacon.example.com")
                .err()
                .map(|fault| fault.code().clone()),
            Some(code!("server.url_invalid"))
        );
    }

    #[test]
    fn only_the_port_https_implies_is_left_out_of_the_address() {
        let authority = |url| Server::new(url).unwrap().authority();

        assert_eq!(
            authority("https://weacon.example.com"),
            "weacon.example.com"
        );
        assert_eq!(
            authority("https://weacon.example.com:8443"),
            "weacon.example.com:8443"
        );
        assert_eq!(authority("http://127.0.0.1:443"), "127.0.0.1:443");
        assert_eq!(authority("http://127.0.0.1"), "127.0.0.1:80");
    }

    #[tokio::test]
    async fn a_request_reaches_a_server_on_this_machine() {
        let local = Local::new().await;
        let serve = async {
            let mut stream = local.accept().await;
            let request = read_request(&mut stream).await;
            answer(&mut stream, "201 Created", &long().to_string()).await;
            request
        };

        let body = json!({ "name": "Studio" });
        let (answered, (head, body)) =
            tokio::join!(posted(local.url(), "/api/machines/pair", body), serve);

        assert_eq!(answered.unwrap(), long());
        assert!(
            head.starts_with("POST /api/machines/pair HTTP/1.1"),
            "{head}"
        );
        assert!(
            head.to_lowercase()
                .contains(&format!("host: 127.0.0.1:{}", local.address.port()))
        );
        assert_eq!(body, r#"{"name":"Studio"}"#);
    }

    #[tokio::test]
    async fn a_refusal_gives_its_status() {
        let local = Local::new().await;
        let serve = async {
            let mut stream = local.accept().await;
            read_request(&mut stream).await;
            answer(
                &mut stream,
                "404 Not Found",
                &error_body(code!("pairing.not_found")),
            )
            .await;
        };

        let (answered, ()) = tokio::join!(posted(local.url(), "/x", Value::Null), serve);

        assert!(
            matches!(&answered, Err(Failure::Refused(404, Some(code))) if *code == code!("pairing.not_found")),
            "{answered:?}"
        );
    }

    #[tokio::test]
    async fn a_certificate_no_public_authority_signed_is_refused() {
        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()));
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![made.cert.der().clone()], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let handshake = async || {
            let (tcp, _) = listener.accept().await.unwrap();
            acceptor.accept(tcp).await
        };
        let server = Server::new(&url).unwrap();

        let (answered, shaken) = tokio::join!(posted(url, "/x", Value::Null), handshake());
        let (socket, shaken_again) = tokio::join!(server.socket("/x", "t0ken"), handshake());

        assert!(
            matches!(answered, Err(Failure::Unreachable(_))),
            "{answered:?}"
        );
        assert!(matches!(socket, Err(Failure::Unreachable(_))));
        shaken.unwrap_err();
        shaken_again.unwrap_err();
    }

    #[tokio::test]
    async fn a_socket_opens_to_a_server_on_this_machine() {
        let local = Local::new().await;
        let server = Server::new(&local.url()).unwrap();
        let serve = async {
            let stream = local.accept().await;
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.send(Wire::text("hi")).await.unwrap();
        };

        let (socket, ()) = tokio::join!(server.socket("/api/machines/connect", "t0ken"), serve);

        let mut socket = socket.unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap(), Wire::text("hi"));
    }

    #[tokio::test]
    async fn a_socket_the_server_refuses_gives_its_status() {
        let local = Local::new().await;
        let server = Server::new(&local.url()).unwrap();
        let serve = async {
            let mut stream = local.accept().await;
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
