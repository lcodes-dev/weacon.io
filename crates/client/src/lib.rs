//! Connects to the core's daemon over its socket and shakes hands with it.

pub mod platform;

use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use weacon_observability::Fault;
use weacon_protocol::{
    App, Envelope, Hello, LineDecoder, Message, PROTOCOL, Welcome, code, encode_line,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const READ_CHUNK: usize = 8192;

/// The daemon's socket in the state folder `state_dir`.
#[must_use]
pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("weacon.sock")
}

/// The daemon's build and capabilities, after a handshake as `me` over the socket at `socket`.
///
/// # Errors
/// When no daemon listens there, it doesn't answer in time, or it refuses the handshake.
pub async fn handshake(socket: &Path, me: App) -> Result<Welcome, Fault> {
    Ok(Connection::open(socket, me).await?.0)
}

/// A raw connection to the daemon at `socket`, before any handshake, such as for protocol tests.
///
/// # Errors
/// When no daemon listens there, or it doesn't answer in time.
pub async fn connect(socket: &Path) -> io::Result<impl AsyncRead + AsyncWrite + Unpin + use<>> {
    tokio::time::timeout(TIMEOUT, platform::connect(socket))
        .await
        .unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::TimedOut)))
}

/// A connection to the daemon after a welcome, which yields what the daemon sends next.
pub struct Connection {
    stream: platform::Stream,
    decoder: LineDecoder,
    pending: VecDeque<Envelope>,
}

impl Connection {
    /// Connects to the daemon at `socket` as `me` and shakes hands, giving the daemon's welcome.
    ///
    /// # Errors
    /// When no daemon listens there, it doesn't answer in time, or it refuses the handshake.
    pub async fn open(socket: &Path, me: App) -> Result<(Welcome, Self), Fault> {
        tokio::time::timeout(TIMEOUT, Self::greet(socket, me))
            .await
            .unwrap_or_else(|_| Err(no_answer(io::Error::from(io::ErrorKind::TimedOut))))
    }

    async fn greet(socket: &Path, me: App) -> Result<(Welcome, Self), Fault> {
        let mut stream = platform::connect(socket).await.map_err(not_running)?;
        let hello = Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol: PROTOCOL,
                app: me,
                capabilities: Vec::new(),
            }),
        };
        let line = encode_line(&hello).map_err(no_answer)?;
        stream.write_all(&line).await.map_err(no_answer)?;
        let mut connection = Self {
            stream,
            decoder: LineDecoder::default(),
            pending: VecDeque::new(),
        };
        let reply = connection
            .next()
            .await?
            .ok_or_else(|| no_answer(io::Error::from(io::ErrorKind::UnexpectedEof)))?;
        Ok((welcomed(reply)?, connection))
    }

    /// The next message from the daemon, skipping frames that don't parse; `None` once it
    /// hangs up.
    ///
    /// # Errors
    /// When reading from the daemon fails.
    pub async fn next(&mut self) -> Result<Option<Message>, Fault> {
        let mut buf = [0; READ_CHUNK];
        loop {
            if let Some(envelope) = self.pending.pop_front() {
                return Ok(Some(envelope.message));
            }
            let read = self.stream.read(&mut buf).await.map_err(no_answer)?;
            let Some(bytes) = buf.get(..read).filter(|bytes| !bytes.is_empty()) else {
                return Ok(None);
            };
            self.pending
                .extend(self.decoder.push(bytes).into_iter().filter_map(Result::ok));
        }
    }
}

fn not_running(error: io::Error) -> Fault {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        Fault::new(
            code!("daemon.not_running"),
            "The daemon isn't running; start it with weacon daemon",
        )
    } else {
        no_answer(error)
    }
}

fn no_answer(error: impl Into<Box<dyn Error + Send + Sync>>) -> Fault {
    Fault::new(code!("daemon.no_answer"), "The daemon didn't answer").because(error)
}

fn welcomed(message: Message) -> Result<Welcome, Fault> {
    match message {
        Message::Welcome(welcome) => Ok(welcome),
        Message::Error(body) if body.code == code!("protocol.unsupported") => Err(Fault::new(
            code!("daemon.other_protocol"),
            "The daemon speaks another protocol version; restart it",
        )),
        Message::Hello(_)
        | Message::Error(_)
        | Message::Receipt(_)
        | Message::Run(_)
        | Message::Unknown => Err(Fault::new(
            code!("daemon.refused"),
            "The daemon refused the handshake",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::task::JoinHandle;
    use weacon_protocol::{ErrorBody, RunEvent, RunUpdate};

    use super::*;
    use crate::platform::Server;

    fn app(name: &str) -> App {
        App {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weacon-client-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A one-connection daemon that reads the hello, then sends a bad frame and `reply`, or hangs up.
    fn serve_once(socket: &Path, reply: Option<Message>) -> JoinHandle<Envelope> {
        let server = Server::bind(socket).unwrap();
        tokio::spawn(serve(server, reply))
    }

    async fn serve(server: Server, reply: Option<Message>) -> Envelope {
        let mut stream = BufReader::new(server.accept().await.unwrap());
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        if let Some(message) = reply {
            stream.write_all(b"not json\n").await.unwrap();
            let reply = encode_line(&Envelope { id: 1, message }).unwrap();
            stream.write_all(&reply).await.unwrap();
        }
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn a_welcome_gives_the_daemons_build_and_capabilities_after_our_hello() {
        let dir = temp_dir("welcome");
        let socket = dir.join("weacon.sock");
        let welcome = Welcome {
            app: app("weacon-core"),
            capabilities: serde_json::from_str(r#"["chat.send"]"#).unwrap(),
        };
        let daemon = serve_once(&socket, Some(Message::Welcome(welcome.clone())));

        assert_eq!(
            handshake(&socket, app("weacon-desktop")).await.unwrap(),
            welcome
        );
        assert_eq!(
            daemon.await.unwrap(),
            Envelope {
                id: 1,
                message: Message::Hello(Hello {
                    protocol: PROTOCOL,
                    app: app("weacon-desktop"),
                    capabilities: Vec::new(),
                }),
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn what_follows_the_welcome_arrives_in_order_until_the_daemon_hangs_up() {
        let dir = temp_dir("stream");
        let socket = dir.join("weacon.sock");
        let server = Server::bind(&socket).unwrap();
        let welcome = Welcome {
            app: app("weacon-core"),
            capabilities: Vec::new(),
        };
        let update = |text: &str| {
            Message::Run(RunUpdate {
                run: serde_json::from_str(r#""r-1""#).unwrap(),
                event: RunEvent::TextDelta {
                    text: text.to_owned(),
                },
            })
        };
        let sent = [
            Message::Welcome(welcome.clone()),
            update("He"),
            update("llo"),
        ];
        let daemon = tokio::spawn(async move {
            let mut stream = BufReader::new(server.accept().await.unwrap());
            stream.read_line(&mut String::new()).await.unwrap();
            let bytes: Vec<u8> = sent
                .into_iter()
                .flat_map(|message| encode_line(&Envelope { id: 0, message }).unwrap())
                .collect();
            stream.write_all(&bytes).await.unwrap();
        });

        let (welcomed, mut connection) = Connection::open(&socket, app("weacon-desktop"))
            .await
            .unwrap();
        daemon.await.unwrap();

        assert_eq!(welcomed, welcome);
        assert_eq!(connection.next().await.unwrap(), Some(update("He")));
        assert_eq!(connection.next().await.unwrap(), Some(update("llo")));
        assert_eq!(connection.next().await.unwrap(), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_daemon_that_hangs_up_gave_no_answer() {
        let dir = temp_dir("hangup");
        let socket = dir.join("weacon.sock");
        let daemon = serve_once(&socket, None);
        let error = handshake(&socket, app("weacon-desktop")).await.unwrap_err();
        drop(daemon.await);

        assert_eq!(error.code().as_str(), "daemon.no_answer");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_refusal_says_why() {
        let refusal = |code| {
            welcomed(Message::Error(ErrorBody {
                code,
                retryable: false,
            }))
            .unwrap_err()
            .code()
            .as_str()
            .to_owned()
        };

        assert_eq!(
            refusal(code!("protocol.unsupported")),
            "daemon.other_protocol"
        );
        assert_eq!(refusal(code!("handshake.required")), "daemon.refused");
        assert_eq!(
            welcomed(Message::Unknown).unwrap_err().code().as_str(),
            "daemon.refused"
        );
    }

    #[tokio::test]
    async fn no_socket_or_a_dead_one_means_no_daemon() {
        let dir = temp_dir("dead");
        let socket = dir.join("weacon.sock");
        drop(Server::bind(&socket).unwrap());

        for path in [socket.as_path(), Path::new("/nonexistent/weacon.sock")] {
            let error = handshake(path, app("weacon-desktop")).await.unwrap_err();

            assert_eq!(error.code().as_str(), "daemon.not_running");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
